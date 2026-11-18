use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use structopt::StructOpt;
use tokio::io::{copy_bidirectional, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::time::timeout;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;
use tokio_socks::tcp::Socks5Stream;

trait RelayStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> RelayStream for T {}

type BoxedStream = Box<dyn RelayStream>;

/// Local TCP forwarder in front of a priority-ordered pool of Monero
/// daemons or electrum servers, dialing every member through a Tor SOCKS
/// proxy. TLS (`ssl://`/`https://`) members terminate their TLS inside the
/// forwarder, so plain-TCP local clients (monero-wallet-rpc, the ASB's
/// electrum client) can use any pool member.
///
/// monero-wallet-rpc points at this proxy instead of a remote daemon.
/// Transparent health probes fail over between pool members when the
/// active daemon's connection drops (onion circuits are fragile), so the
/// wallet RPC never sits in its multi-minute silent reconnect backoff.
/// With --pinned the pool is never switched: the configured daemon is the
/// only member (users running their own node keep full control), while
/// probes still report its health as DPP_UNHEALTHY.
///
/// Beyond reachability, a staleness check compares the active member's
/// block tip against the other members. A "healthy" server can still be
/// stuck hours behind (both of the pool's indexers once served a mined
/// transaction as unconfirmed at a frozen tip). When another member is
/// `--lag_blocks` ahead, the active member is switched and all live relay
/// connections are closed so clients reconnect through the fresh member.
///
/// A third check catches a member that passes the cheap probes but never
/// answers real requests (swap 927e04b3: a node that answered get_height in
/// a second left a wallet sync stalled for 16 minutes). For Monero pools, a
/// request that gets no response byte at all for `--stall-secs` is a stall;
/// `--stall-rotate-after` stalls in a row (any response clears the count)
/// switch to the next healthy member and demote the stalling one for
/// `--demote-secs`, so failback does not send traffic straight back to it.
/// A node that stalls in the middle of a response is not covered: that still
/// depends on the client's own timeouts.
#[derive(Debug, StructOpt)]
struct Args {
    /// Local listen address, e.g. 127.0.0.1:38089
    #[structopt(long)]
    listen: String,

    /// Tor SOCKS5 proxy address
    #[structopt(long)]
    tor_socks: String,

    /// Daemon URLs in priority order (first = preferred), e.g.
    /// http://host.onion:38089,http://other.host:18089
    #[structopt(long, use_delimiter = true)]
    daemons: Vec<String>,

    /// Never fail over to other pool members
    #[structopt(long)]
    pinned: bool,

    /// Pool member protocol: monero (daemon RPC) or electrum
    #[structopt(long, default_value = "monero", possible_values = &["monero", "electrum"])]
    kind: String,

    /// Seconds between health probes of the active daemon
    #[structopt(long, default_value = "30")]
    probe_secs: u64,

    /// Consecutive failed probes before failing over
    #[structopt(long, default_value = "2")]
    max_probe_fails: u32,

    /// Seconds between re-probes of higher-priority members for failback
    #[structopt(long, default_value = "300")]
    failback_probe_secs: u64,

    /// Seconds between tip-staleness checks of the pool (0 disables)
    #[structopt(long, default_value = "90")]
    lag_probe_secs: u64,

    /// Block-tip lag that triggers a switch to a fresher pool member
    #[structopt(long, default_value = "2")]
    lag_blocks: u64,

    /// Seconds a request may get no response byte before it counts as a stall
    /// (0 disables; Electrum pools never use it)
    #[structopt(long, default_value = "75")]
    stall_secs: u64,

    /// Stalls in a row before switching to the next healthy member
    #[structopt(long, default_value = "2")]
    stall_rotate_after: usize,

    /// Seconds a stalling member stays demoted (skipped by failover and failback)
    #[structopt(long, default_value = "600")]
    demote_secs: u64,
}

struct PoolState {
    daemons: Vec<String>,
    tor_socks: String,
    pinned: bool,
    electrum: bool,
    max_probe_fails: u32,
    current: AtomicUsize,
    probe_fails: AtomicUsize,
    epoch: AtomicU64,
    active_conns: AtomicUsize,
    switch_notify: Notify,
    stall_limit: Duration,
    stall_rotate_after: usize,
    demote_for: Duration,
    stalls: AtomicUsize,
    demotions: Mutex<Demotions>,
}

/// Members that stalled recently: skipped when choosing where to fail over or back to.
struct Demotions {
    until: Vec<Option<Instant>>,
}

impl Demotions {
    fn new(members: usize) -> Self {
        Demotions { until: vec![None; members] }
    }

    fn demote(&mut self, index: usize, now: Instant, for_: Duration) {
        if let Some(slot) = self.until.get_mut(index) {
            *slot = Some(now + for_);
        }
    }

    fn is_demoted(&self, index: usize, now: Instant) -> bool {
        matches!(self.until.get(index), Some(Some(until)) if *until > now)
    }
}

/// Whether enough stalls in a row happened to switch away from the active member.
fn rotate_after_stall(stalls: usize, rotate_after: usize, pinned: bool, members: usize) -> bool {
    !pinned && members > 1 && rotate_after > 0 && stalls >= rotate_after
}

#[derive(Debug, PartialEq, Eq)]
enum RelayEnd {
    /// One side closed or failed.
    Closed,
    /// A request got no response byte within the stall limit.
    Stalled,
}

/// Relays both ways like `copy_bidirectional`, but ends with [`RelayEnd::Stalled`] when the
/// client sent something and the daemon has not sent a single byte back for `stall`. Idle time
/// between requests never counts. `on_daemon_bytes` runs whenever the daemon sends data.
async fn relay_with_stall_watchdog<C, D, F>(
    client: C,
    daemon: D,
    stall: Duration,
    mut on_daemon_bytes: F,
) -> RelayEnd
where
    C: AsyncRead + AsyncWrite + Unpin,
    D: AsyncRead + AsyncWrite + Unpin,
    F: FnMut(),
{
    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (mut daemon_read, mut daemon_write) = tokio::io::split(daemon);
    let mut from_client = vec![0u8; 16 * 1024];
    let mut from_daemon = vec![0u8; 16 * 1024];
    let mut waiting_since: Option<tokio::time::Instant> = None;

    loop {
        let stalled = async {
            match waiting_since {
                Some(since) => tokio::time::sleep_until(since + stall).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            read = client_read.read(&mut from_client) => match read {
                Ok(0) | Err(_) => return RelayEnd::Closed,
                Ok(n) => {
                    if daemon_write.write_all(&from_client[..n]).await.is_err() {
                        return RelayEnd::Closed;
                    }
                    if waiting_since.is_none() {
                        waiting_since = Some(tokio::time::Instant::now());
                    }
                }
            },
            read = daemon_read.read(&mut from_daemon) => match read {
                Ok(0) | Err(_) => return RelayEnd::Closed,
                Ok(n) => {
                    if client_write.write_all(&from_daemon[..n]).await.is_err() {
                        return RelayEnd::Closed;
                    }
                    waiting_since = None;
                    on_daemon_bytes();
                }
            },
            _ = stalled => return RelayEnd::Stalled,
        }
    }
}

impl PoolState {
    fn is_demoted(&self, index: usize) -> bool {
        self.demotions.lock().map(|d| d.is_demoted(index, Instant::now())).unwrap_or(false)
    }

    fn demote(&self, index: usize) {
        if let Ok(mut demotions) = self.demotions.lock() {
            demotions.demote(index, Instant::now(), self.demote_for);
        }
    }

    fn current(&self) -> usize {
        self.current.load(Ordering::SeqCst)
    }

    fn set_current(&self, index: usize, reason: &str) {
        self.probe_fails.store(0, Ordering::SeqCst);
        self.stalls.store(0, Ordering::SeqCst);
        let previous = self.current.swap(index, Ordering::SeqCst);
        println!(
            "DPP_ACTIVE daemon {} (was {})",
            self.daemons[index], reason
        );
        if previous != index {
            self.epoch.fetch_add(1, Ordering::SeqCst);
            println!(
                "DPP_SWITCHED from {} to {}",
                self.daemons[previous], self.daemons[index]
            );
            self.switch_notify.notify_waiters();
        }
    }
}

fn daemon_target(url: &str) -> (&str, u16) {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ssl://"))
        .or_else(|| url.strip_prefix("tcp://"))
        .unwrap_or(url);
    let authority = without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme);
    match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().unwrap_or(18081)),
        None => (authority, 18081),
    }
}

fn wants_tls(url: &str) -> bool {
    url.starts_with("ssl://") || url.starts_with("https://")
}

fn tls_connector() -> TlsConnector {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

/// Dial one pool member through the Tor SOCKS proxy, terminating TLS for
/// ssl:// members so local plain-TCP clients can use them.
async fn dial_one(tor_socks: &str, url: &str) -> Option<BoxedStream> {
    let (host, port) = daemon_target(url);
    let socks_addr: std::net::SocketAddr = tor_socks
        .parse()
        .expect("tor socks address must be ip:port");

    let dial = async {
        let stream = Socks5Stream::connect(socks_addr, (host, port))
            .await
            .ok()?
            .into_inner();
        if !wants_tls(url) {
            return Some(Box::new(stream) as BoxedStream);
        }
        let server_name = ServerName::try_from(host.to_owned()).ok()?;
        tls_connector()
            .connect(server_name, stream)
            .await
            .ok()
            .map(|stream| Box::new(stream) as BoxedStream)
    };

    timeout(dial_timeout(host), dial).await.ok().flatten()
}

/// How long one dial through Tor may take. The first connection to an onion service builds a
/// rendezvous circuit, which on a fresh Tor often takes longer than 15 s: with the old 15 s limit a
/// new install's first Monero wallet sync failed with "no connection to daemon" while the node was
/// up (2026-11-11, throwaway first launch). Clearnet hosts go through a Tor exit, which is quicker.
fn dial_timeout(host: &str) -> Duration {
    if host.ends_with(".onion") {
        Duration::from_secs(60)
    } else {
        Duration::from_secs(30)
    }
}

/// Health probe of one pool member: returns the member's block tip when
/// reachable, None when the probe fails. Monero daemons must answer HTTP
/// POST /get_height with height+OK; electrum servers must answer the
/// blockchain.headers.subscribe JSON-RPC request with a height.
///
/// /get_height is the first endpoint the protocol-5 output scan calls. Some
/// frontends serve /get_info but reject /get_height, so probing /get_info
/// alone can select a member that passes health checks and then fails after
/// the Bitcoin lock is broadcast.
async fn probe_daemon(tor_socks: &str, url: &str, electrum: bool) -> Option<u64> {
    let mut stream = dial_one(tor_socks, url).await?;

    let exchange = async {
        if electrum {
            let request =
                "{\"id\":1,\"method\":\"blockchain.headers.subscribe\",\"params\":[]}\n";
            if stream.write_all(request.as_bytes()).await.is_err() {
                return None;
            }
            let mut response = Vec::new();
            let mut chunk = [0u8; 1024];
            while response.len() <= 8192 {
                match stream.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(read) => {
                        response.extend_from_slice(&chunk[..read]);
                        if response.contains(&b'\n') {
                            break;
                        }
                    }
                    Err(_) => return None,
                }
            }
            let body = String::from_utf8_lossy(&response);
            match serde_json::from_str::<serde_json::Value>(body.lines().next().unwrap_or_default())
            {
                Ok(parsed) => parsed["result"]["height"].as_u64(),
                Err(_) => None,
            }
        } else {
            let (host, _) = daemon_target(url);
            let request = format!(
                "POST /get_height HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            if stream.write_all(request.as_bytes()).await.is_err() {
                return None;
            }
            let mut response = Vec::new();
            if stream.read_to_end(&mut response).await.is_err() {
                return None;
            }
            let body = String::from_utf8_lossy(&response);
            let json_start = body.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
            let parsed: serde_json::Value = match serde_json::from_str(&body[json_start..]) {
                Ok(parsed) => parsed,
                Err(_) => return None,
            };
            if parsed["status"] == "OK" {
                parsed["height"].as_u64()
            } else {
                None
            }
        }
    };

    timeout(Duration::from_secs(15), exchange).await.unwrap_or(None)
}

async fn dial_daemon(pool: &Arc<PoolState>) -> Option<BoxedStream> {
    let start = pool.current();
    let count = pool.daemons.len();
    for offset in 0..count {
        let index = (start + offset) % count;
        if let Some(stream) = dial_one(&pool.tor_socks, &pool.daemons[index]).await {
            if offset > 0 {
                pool.set_current(index, "after live dial failure of the active daemon");
            }
            return Some(stream);
        }
        println!("DPP_DIAL_FAILED daemon {}", pool.daemons[index]);
    }
    None
}

struct ConnGuard(Arc<PoolState>);

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.active_conns.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn handle_connection(pool: Arc<PoolState>, mut client: TcpStream) {
    pool.active_conns.fetch_add(1, Ordering::SeqCst);
    let _guard = ConnGuard(pool.clone());
    let epoch = pool.epoch.load(Ordering::SeqCst);

    let Some(mut daemon) = dial_daemon(&pool).await else {
        println!("DPP_EXHAUSTED all pool members unreachable");
        return;
    };

    if pool.epoch.load(Ordering::SeqCst) != epoch {
        println!("DPP_RESET closing relay after member switch during dial");
        return;
    }

    if pool.electrum || pool.stall_limit.is_zero() {
        tokio::select! {
            result = copy_bidirectional(&mut client, &mut daemon) => {
                if let Err(error) = result {
                    println!("DPP_STREAM_ERROR {error}");
                }
            }
            _ = pool.switch_notify.notified() => {
                println!("DPP_RESET closing relay so the client reconnects through the new active member");
            }
        }
        return;
    }

    // dial_daemon already switched the active member when it had to use another one
    let served_by = pool.current();
    let relay = relay_with_stall_watchdog(&mut client, &mut daemon, pool.stall_limit, || {
        pool.stalls.store(0, Ordering::SeqCst);
    });
    tokio::select! {
        end = relay => {
            if end == RelayEnd::Stalled {
                on_stall(&pool, served_by).await;
            }
        }
        _ = pool.switch_notify.notified() => {
            println!("DPP_RESET closing relay so the client reconnects through the new active member");
        }
    }
}

/// A request got no answer within the stall limit: count it, and after enough in a row demote the
/// active member and switch to the next healthy one.
async fn on_stall(pool: &Arc<PoolState>, index: usize) {
    let stalls = pool.stalls.fetch_add(1, Ordering::SeqCst) + 1;
    println!(
        "DPP_STALL daemon {} gave no answer within {}s ({} stalls in a row)",
        pool.daemons[index],
        pool.stall_limit.as_secs(),
        stalls
    );
    if !rotate_after_stall(stalls, pool.stall_rotate_after, pool.pinned, pool.daemons.len()) {
        return;
    }
    if pool.current() != index {
        return; // another connection already switched
    }
    pool.demote(index);
    let count = pool.daemons.len();
    for offset in 1..count {
        let candidate = (index + offset) % count;
        if pool.is_demoted(candidate) {
            continue;
        }
        if probe_daemon(&pool.tor_socks, &pool.daemons[candidate], pool.electrum)
            .await
            .is_some()
        {
            pool.set_current(candidate, &format!("after {} stalled requests of the active daemon", stalls));
            return;
        }
    }
    println!("DPP_STALL_NO_ALTERNATIVE no healthy member to switch to; staying on {}", pool.daemons[index]);
}

async fn health_loop(pool: Arc<PoolState>, probe_secs: u64) {
    loop {
        tokio::time::sleep(Duration::from_secs(probe_secs)).await;

        let index = pool.current();
        let healthy = probe_daemon(&pool.tor_socks, &pool.daemons[index], pool.electrum)
            .await
            .is_some();

        if healthy {
            if pool.probe_fails.fetch_and(0, Ordering::SeqCst) > 0 {
                println!("DPP_RECOVERED daemon {}", pool.daemons[index]);
            }
            continue;
        }

        let fails = pool.probe_fails.fetch_add(1, Ordering::SeqCst) + 1;
        println!(
            "DPP_UNHEALTHY daemon {} ({} consecutive failed probes)",
            pool.daemons[index], fails
        );

        if pool.pinned || fails < pool.max_probe_fails as usize || pool.daemons.len() == 1 {
            continue;
        }

        let count = pool.daemons.len();
        for offset in 1..count {
            let candidate = (index + offset) % count;
            if pool.is_demoted(candidate) {
                continue;
            }
            if probe_daemon(&pool.tor_socks, &pool.daemons[candidate], pool.electrum)
                .await
                .is_some()
            {
                pool.set_current(
                    candidate,
                    &format!("after {} failed probes of the active daemon", fails),
                );
                break;
            }
        }
    }
}

async fn failback_loop(pool: Arc<PoolState>, failback_probe_secs: u64) {
    loop {
        tokio::time::sleep(Duration::from_secs(failback_probe_secs)).await;

        if pool.pinned || pool.current() == 0 {
            continue;
        }

        for candidate in 0..pool.current() {
            if pool.is_demoted(candidate) {
                continue; // it stalled recently: the cheap probe would send traffic straight back
            }
            if probe_daemon(&pool.tor_socks, &pool.daemons[candidate], pool.electrum)
                .await
                .is_some()
            {
                pool.set_current(candidate, "failback to higher-priority daemon");
                break;
            }
        }
    }
}

/// Watch the pool for tip lag: a member that answers RPC but sits behind
/// the others still serves stale history (the indexer-lag stall class).
/// When the active member is `lag_blocks` behind any other member, switch
/// and reset live connections so clients reconnect through the fresh one.
/// Pinned pools are left alone entirely — no probes of unselected members.
async fn staleness_loop(pool: Arc<PoolState>, lag_probe_secs: u64, lag_blocks: u64) {
    loop {
        tokio::time::sleep(Duration::from_secs(lag_probe_secs)).await;

        if pool.pinned
            || pool.daemons.len() < 2
            || pool.active_conns.load(Ordering::SeqCst) == 0
        {
            continue;
        }

        let index = pool.current();
        let Some(active_tip) =
            probe_daemon(&pool.tor_socks, &pool.daemons[index], pool.electrum).await
        else {
            continue;
        };

        let count = pool.daemons.len();
        for offset in 1..count {
            let candidate = (index + offset) % count;
            if pool.is_demoted(candidate) {
                continue;
            }
            if let Some(tip) =
                probe_daemon(&pool.tor_socks, &pool.daemons[candidate], pool.electrum).await
            {
                if tip >= active_tip.saturating_add(lag_blocks) {
                    println!(
                        "DPP_STALE active daemon {} tip {} is behind {} tip {} — switching",
                        pool.daemons[index], active_tip, pool.daemons[candidate], tip
                    );
                    pool.set_current(
                        candidate,
                        &format!("indexer tip lag ({} vs {})", active_tip, tip),
                    );
                    break;
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    swap::util::install_default_crypto_provider();
    let args = Args::from_args();

    if args.daemons.is_empty() {
        return Err("no daemons configured".into());
    }

    let member_count = args.daemons.len();
    let pool = Arc::new(PoolState {
        daemons: args.daemons,
        tor_socks: args.tor_socks,
        pinned: args.pinned,
        electrum: args.kind == "electrum",
        max_probe_fails: args.max_probe_fails,
        current: AtomicUsize::new(0),
        probe_fails: AtomicUsize::new(0),
        epoch: AtomicU64::new(0),
        active_conns: AtomicUsize::new(0),
        switch_notify: Notify::new(),
        stall_limit: Duration::from_secs(args.stall_secs),
        stall_rotate_after: args.stall_rotate_after,
        demote_for: Duration::from_secs(args.demote_secs),
        stalls: AtomicUsize::new(0),
        demotions: Mutex::new(Demotions::new(member_count)),
    });

    println!(
        "DPP_START listening on {} tor_socks {} pinned {} kind {} lag_probe_secs {} lag_blocks {} stall_secs {} daemons {:?}",
        args.listen,
        pool.tor_socks,
        pool.pinned,
        args.kind,
        args.lag_probe_secs,
        args.lag_blocks,
        pool.stall_limit.as_secs(),
        pool.daemons
    );

    let listener = TcpListener::bind(&args.listen).await?;

    let health_pool = pool.clone();
    tokio::spawn(health_loop(health_pool, args.probe_secs));

    let failback_pool = pool.clone();
    tokio::spawn(failback_loop(failback_pool, args.failback_probe_secs));

    if args.lag_probe_secs > 0 {
        let staleness_pool = pool.clone();
        tokio::spawn(staleness_loop(
            staleness_pool,
            args.lag_probe_secs,
            args.lag_blocks,
        ));
    }

    loop {
        let (client, _peer) = listener.accept().await?;
        let conn_pool = pool.clone();
        tokio::spawn(handle_connection(conn_pool, client));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onion_dials_get_time_to_build_a_rendezvous_circuit() {
        let (host, _) = daemon_target("http://plowsof3t5hogddwabaeiyrno25efmzfxyro2vligremt7sxpsclfaid.onion:38089");
        assert!(dial_timeout(host) >= Duration::from_secs(60));
    }

    #[test]
    fn clearnet_dials_through_a_tor_exit_get_more_than_fifteen_seconds() {
        let (host, _) = daemon_target("ssl://electrum.example.org:50002");
        assert!(dial_timeout(host) > Duration::from_secs(15));
        assert!(dial_timeout(host) < dial_timeout("abc.onion"));
    }

    // ---- the stall watchdog ----

    #[tokio::test(start_paused = true)]
    async fn data_passes_both_ways_and_the_relay_ends_when_a_side_closes() {
        let (mut client_side, relay_client) = tokio::io::duplex(1024);
        let (relay_daemon, mut daemon_side) = tokio::io::duplex(1024);
        let relay = tokio::spawn(relay_with_stall_watchdog(relay_client, relay_daemon, Duration::from_secs(75), || {}));

        client_side.write_all(b"request").await.unwrap();
        let mut request = [0u8; 7];
        daemon_side.read_exact(&mut request).await.unwrap();
        assert_eq!(&request, b"request");
        daemon_side.write_all(b"answer").await.unwrap();
        let mut answer = [0u8; 6];
        client_side.read_exact(&mut answer).await.unwrap();
        assert_eq!(&answer, b"answer");

        drop(client_side);
        assert_eq!(relay.await.unwrap(), RelayEnd::Closed);
    }

    #[tokio::test(start_paused = true)]
    async fn a_node_that_never_answers_a_request_is_reported_as_stalled() {
        let (mut client_side, relay_client) = tokio::io::duplex(1024);
        let (relay_daemon, _daemon_side) = tokio::io::duplex(1024); // open, but silent
        let relay = tokio::spawn(relay_with_stall_watchdog(relay_client, relay_daemon, Duration::from_secs(75), || {}));

        client_side.write_all(b"get_blocks").await.unwrap();
        assert_eq!(relay.await.unwrap(), RelayEnd::Stalled);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_answer_is_not_a_stall_and_idle_time_after_it_never_is() {
        let (mut client_side, relay_client) = tokio::io::duplex(1024);
        let (relay_daemon, mut daemon_side) = tokio::io::duplex(1024);
        let answered = Arc::new(AtomicUsize::new(0));
        let answered_in_relay = answered.clone();
        let relay = tokio::spawn(relay_with_stall_watchdog(relay_client, relay_daemon, Duration::from_secs(75), move || {
            answered_in_relay.fetch_add(1, Ordering::SeqCst);
        }));

        client_side.write_all(b"request").await.unwrap();
        let slow_node = tokio::spawn(async move {
            let mut request = [0u8; 7];
            daemon_side.read_exact(&mut request).await.unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await; // slow, but under the limit
            daemon_side.write_all(b"answer").await.unwrap();
            daemon_side
        });
        let mut answer = [0u8; 6];
        client_side.read_exact(&mut answer).await.unwrap();
        assert_eq!(&answer, b"answer");
        let _daemon_side = slow_node.await.unwrap();

        tokio::time::sleep(Duration::from_secs(600)).await; // an idle keep-alive connection
        drop(client_side);
        assert_eq!(relay.await.unwrap(), RelayEnd::Closed);
        assert!(answered.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn switching_needs_enough_stalls_in_a_row_and_a_pool_with_somewhere_to_go() {
        assert!(!rotate_after_stall(1, 2, false, 5));
        assert!(rotate_after_stall(2, 2, false, 5));
        assert!(rotate_after_stall(3, 2, false, 5));
        assert!(!rotate_after_stall(9, 2, true, 5), "a pinned pool never switches");
        assert!(!rotate_after_stall(9, 2, false, 1), "a single member has nowhere to go");
        assert!(!rotate_after_stall(9, 0, false, 5), "zero disables switching");
    }

    #[test]
    fn a_demoted_member_is_skipped_until_its_time_is_up() {
        let mut demotions = Demotions::new(3);
        let now = Instant::now();
        assert!(!demotions.is_demoted(1, now));
        demotions.demote(1, now, Duration::from_secs(600));
        assert!(demotions.is_demoted(1, now));
        assert!(demotions.is_demoted(1, now + Duration::from_secs(599)));
        assert!(!demotions.is_demoted(1, now + Duration::from_secs(601)));
        assert!(!demotions.is_demoted(0, now), "only the stalling member is demoted");
        demotions.demote(7, now, Duration::from_secs(600)); // out of range: ignored, no panic
        assert!(!demotions.is_demoted(7, now));
    }
}
