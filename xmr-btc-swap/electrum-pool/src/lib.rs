use bdk_electrum::BdkElectrumClient;
use bdk_electrum::electrum_client::{
    Client, Config, ConfigBuilder, ElectrumApi, Error, Socks5Config,
};
use bitcoin::Transaction;
use futures::stream::{FuturesUnordered, StreamExt};
use once_cell::sync::OnceCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use std::time::Instant;
use tokio::task::spawn_blocking;
use tracing::{debug, error, instrument, trace, warn};

/// How long a session that blew the wall-clock deadline stays out of
/// rotation before a fresh connection is attempted.
const HUNG_CLIENT_COOLDOWN: Duration = Duration::from_secs(60);

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// A single endpoint call at least this slow is logged with its endpoint.
const SLOW_ENDPOINT_CALL: Duration = Duration::from_secs(5);

/// Run `f(client)` with a wall-clock deadline.
///
/// `rust-electrum-client` does socket I/O on its own poller thread and
/// hands results back over a channel. If that thread dies, `recv` blocks
/// forever and the per-request socket timeout never fires. Waiting with a
/// deadline turns that permanent hang into a failoverable error.
///
/// On timeout the worker thread is leaked (a blocking call cannot be
/// cancelled); the caller marks the session as hung so it is not reused.
fn is_timeout(err: &Error) -> bool {
    matches!(err, Error::IOError(e) if e.kind() == std::io::ErrorKind::TimedOut)
}

/// Whether `err` means the TCP session itself is unusable (read timed out,
/// peer reset, broken pipe, ...), as opposed to the server rejecting a request.
/// The raw client wraps these in `AllAttemptsErrored`, e.g. the swap f7a162f7
/// failure `Made one or multiple attempts, all errored: Resource temporarily
/// unavailable (os error 35)`, so nested errors are inspected too. Retrying on
/// the same dead socket cannot succeed; the session must be replaced.
fn is_dead_session_error(err: &Error) -> bool {
    use std::io::ErrorKind::{
        BrokenPipe, ConnectionAborted, ConnectionReset, NotConnected, TimedOut, UnexpectedEof,
        WouldBlock,
    };
    fn io_is_dead(error: &std::io::Error) -> bool {
        matches!(
            error.kind(),
            TimedOut
                | WouldBlock
                | ConnectionReset
                | ConnectionAborted
                | BrokenPipe
                | UnexpectedEof
                | NotConnected
        )
    }
    match err {
        Error::IOError(error) => io_is_dead(error),
        Error::SharedIOError(error) => io_is_dead(error),
        Error::AllAttemptsErrored(errors) => errors.iter().any(is_dead_session_error),
        _ => false,
    }
}

fn run_with_deadline<C, F, T>(client: Arc<C>, f: F, deadline: Duration) -> Result<T, Error>
where
    C: Send + Sync + 'static,
    F: Fn(&C) -> Result<T, Error> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("electrum-call".to_string())
        .spawn(move || {
            let _ = tx.send(f(&client));
        })
        .map_err(|e| Error::IOError(std::io::Error::other(e)))?;

    match rx.recv_timeout(deadline) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(Error::IOError(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "Electrum call exceeded the {}s wall-clock deadline",
                deadline.as_secs()
            ),
        ))),
        Err(RecvTimeoutError::Disconnected) => Err(Error::IOError(std::io::Error::other(
            "Electrum call thread terminated without returning a result",
        ))),
    }
}

/// Failover pool for Electrum connections.
///
/// The first URL is considered the primary server. Every sequential
/// request starts at the primary and fails over through the remaining
/// servers in order until the provided closure succeeds or all servers
/// have been exhausted. Failing over to the next server happens
/// immediately; a backoff is only applied once a full pass over all
/// servers has failed.
///
/// Sequential calls fail over on *every* error, including protocol
/// errors, because those may be server deficiencies ("txindex not
/// ready", unsupported endpoint) that another server does not have.
///
/// Clients are created lazily on first use to avoid blocking during initialization.
pub struct ElectrumBalancer<C = BdkElectrumClient<Client>>
where
    C: ElectrumClientLike,
{
    urls: Vec<String>,
    #[allow(clippy::type_complexity)]
    clients: Arc<RwLock<Vec<Arc<OnceCell<Arc<C>>>>>>,
    /// Epoch milliseconds of the last wall-clock timeout per client index.
    /// A non-zero value means the session hung; it is discarded after a
    /// cooldown so a dead channel cannot be reused.
    hung_at: Arc<Vec<AtomicU64>>,
    config: ElectrumBalancerConfig,
    factory: Arc<dyn ElectrumClientFactory<C> + Send + Sync>,
}

impl<C> ElectrumBalancer<C>
where
    C: ElectrumClientLike,
{
    /// Helper function to get or initialize a client for a given index
    fn get_or_init_client_sync(&self, idx: usize) -> Result<Arc<C>, Error> {
        if self.client_recently_hung(idx) {
            // Deliberately not `TimedOut`: this fast-fail must not re-arm the
            // cooldown that produced it, otherwise the session would never be
            // reset and the balancer would spin on failures forever.
            return Err(Error::IOError(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!(
                    "Electrum session for {} hung recently; cooling down",
                    self.urls.get(idx).map(String::as_str).unwrap_or("?")
                ),
            )));
        }

        // We wrap this in a closure to only lock the RwLock for as long as needed
        let (client_once_cell, url, config, factory) = {
            let clients = self.clients.read().expect("rwlock poisoned").clone();

            if idx >= clients.len() {
                return Err(Error::IOError(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("Index {} out of bounds for {} clients", idx, clients.len()),
                )));
            }

            let once_cell = clients[idx].clone();
            let url = self.urls[idx].clone();
            let config = self.config.clone();
            let factory = self.factory.clone();

            (once_cell, url, config, factory)
        };

        let client = client_once_cell.get_or_try_init(|| {
            // Connecting to an onion server can take a minute on a cold Tor, and no other
            // timer starts before the connection exists (a 60 s sync went unexplained).
            let started = Instant::now();
            let result = factory.create_client(&url, &config);
            match &result {
                Ok(_) => tracing::info!(
                    server_url = %url,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "Electrum connection established"
                ),
                Err(error) => warn!(
                    server_url = %url,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    ?error,
                    "Electrum connection failed"
                ),
            }
            result
        })?;

        Ok(client.clone())
    }

    fn call_deadline(&self) -> Duration {
        Duration::from_secs(self.config.call_timeout.max(1) as u64)
    }

    fn mark_client_hung(&self, idx: usize) {
        // With a single endpoint there is nothing to fail over to, so a
        // cooldown only delays the next real attempt (the taker sat in
        // "hung recently; cooling down" for minutes). Drop the session and
        // let the next call reconnect immediately.
        if self.urls.len() == 1 {
            self.reset_client(idx);
            return;
        }
        if let Some(slot) = self.hung_at.get(idx) {
            slot.store(now_millis(), Ordering::SeqCst);
        }
    }

    /// Discards the session at `idx` so the next call reconnects from scratch.
    fn reset_client(&self, idx: usize) {
        if let Ok(mut clients) = self.clients.write() {
            if idx < clients.len() {
                clients[idx] = Arc::new(OnceCell::new());
            }
        }
    }

    /// Returns `true` while the session is cooling down. Once the cooldown
    /// elapses the dead session is dropped so the next attempt reconnects.
    fn client_recently_hung(&self, idx: usize) -> bool {
        let Some(slot) = self.hung_at.get(idx) else {
            return false;
        };
        let last = slot.load(Ordering::SeqCst);
        if last == 0 {
            return false;
        }
        if now_millis().saturating_sub(last) < HUNG_CLIENT_COOLDOWN.as_millis() as u64 {
            return true;
        }
        if let Ok(mut clients) = self.clients.write() {
            if idx < clients.len() {
                clients[idx] = Arc::new(OnceCell::new());
            }
        }
        slot.store(0, Ordering::SeqCst);
        false
    }

    async fn get_or_init_client_async(&self, idx: usize) -> Result<Arc<C>, Error> {
        // A cooling-down endpoint fails fast, exactly like the sync path, so a
        // parallel quorum call returns the healthy servers' answers instead of
        // waiting out the hung one. (A single endpoint has no cooldown, see
        // `mark_client_hung`.)
        let balancer = self.clone();
        spawn_blocking(move || balancer.get_or_init_client_sync(idx))
            .await
            .map_err(|e| Error::IOError(std::io::Error::other(e.to_string())))?
    }

    /// Create a new balancer from a list of Electrum URLs with default configuration.
    pub async fn new_with_factory(
        urls: Vec<String>,
        factory: Arc<dyn ElectrumClientFactory<C> + Send + Sync>,
    ) -> Result<Self, Error> {
        Self::new_with_config_and_factory(urls, ElectrumBalancerConfig::default(), factory).await
    }

    /// Get any client from the balancer
    pub async fn get_any_client(&self) -> Result<Arc<C>, Error> {
        // Try to initialize any client
        for idx in 0..self.client_count() {
            match self.get_or_init_client_async(idx).await {
                Ok(client) => return Ok(client),
                Err(e) => {
                    trace!(
                        server_url = self.urls[idx],
                        error = ?e,
                        "Failed to initialize client, trying next client"
                    );
                }
            }
        }

        // Return error if no client could be initialized
        Err(Error::IOError(std::io::Error::other(
            "No client could be initialized",
        )))
    }

    /// Create a new balancer from a list of Electrum URLs with custom configuration.
    /// Clients are initialized lazily on first use.
    pub async fn new_with_config_and_factory(
        urls: Vec<String>,
        config: ElectrumBalancerConfig,
        factory: Arc<dyn ElectrumClientFactory<C> + Send + Sync>,
    ) -> Result<Self, Error> {
        if urls.is_empty() {
            return Err(Error::Protocol("No Electrum URLs provided".into()));
        }

        debug!(
            servers = ?urls,
            server_count = urls.len(),
            timeout_seconds = config.request_timeout,
            min_retries = config.min_retries,
            "Initializing Electrum load balancer"
        );

        // Create OnceCell containers for each URL - clients will be created on first use
        let clients: Vec<Arc<OnceCell<Arc<C>>>> =
            urls.iter().map(|_| Arc::new(OnceCell::new())).collect();

        let hung_at: Arc<Vec<AtomicU64>> =
            Arc::new(urls.iter().map(|_| AtomicU64::new(0)).collect());

        Ok(Self {
            urls,
            clients: Arc::new(RwLock::new(clients)),
            hung_at,
            config,
            factory,
        })
    }

    /// Get the number of URLs (potential clients)
    pub fn client_count(&self) -> usize {
        self.urls.len()
    }

    /// Execute the given closure using one of the Electrum clients asynchronously.
    ///
    /// If the closure returns an I/O error or certificate error the balancer will try the next
    /// node until all nodes have been exhausted. The last encountered error
    /// is returned in that case.
    #[instrument(level = "debug", skip(self, f), fields(operation = kind, total_urls = self.urls.len(), total_clients = self.client_count()))]
    pub async fn call<F, T>(&self, kind: &str, f: F) -> Result<T, Error>
    where
        F: Fn(&C) -> Result<T, Error> + Send + Sync + Clone + 'static,
        T: Send + 'static,
    {
        let balancer = self.clone();
        let kind = kind.to_string();

        match spawn_blocking(move || balancer.call_sync(&kind, f)).await {
            Ok(result) => result.map_err(|multi_error| multi_error.into()),
            Err(e) => Err(Error::IOError(std::io::Error::other(e.to_string()))),
        }
    }

    /// Execute the given closure using one of the Electrum clients asynchronously,
    /// returning the full MultiError for detailed error analysis.
    ///
    /// Unlike `call_async`, this method exposes the full MultiError containing all
    /// individual failures, allowing the caller to inspect and make decisions based
    /// on the specific types of errors encountered.
    #[instrument(level = "debug", skip(self, f), fields(operation = kind, total_clients = self.client_count()))]
    pub async fn call_async_with_multi_error<F, T>(&self, kind: &str, f: F) -> Result<T, MultiError>
    where
        F: Fn(&C) -> Result<T, Error> + Send + Sync + Clone + 'static,
        T: Send + 'static,
    {
        let balancer = self.clone();
        let kind_string = kind.to_string();
        let kind_for_error = kind.to_string();

        match spawn_blocking(move || balancer.call_sync(&kind_string, f)).await {
            Ok(result) => result,
            Err(e) => {
                let context =
                    format!("Failed to spawn blocking task for operation '{kind_for_error}'");
                let error = Error::IOError(std::io::Error::other(e.to_string()));
                Err(MultiError::new(vec![error], context))
            }
        }
    }

    /// Execute the given closure using one of the Electrum clients synchronously.
    ///
    /// This version blocks for client creation if needed but executes the request synchronously.
    /// Used for implementing the ElectrumApi trait.
    ///
    /// If the closure returns an I/O error or certificate error the balancer will try the next
    /// node until all nodes have been exhausted. The last encountered error
    /// is returned in that case.
    ///
    /// Returns `MultiError` containing all individual failures, which can be inspected
    /// by the caller or automatically converted to a single `Error` for compatibility.
    #[instrument(level = "debug", skip(self, f), fields(operation = kind, total_clients = self.client_count(), min_retries = self.config.min_retries))]
    fn call_sync<F, T>(&self, kind: &str, f: F) -> Result<T, MultiError>
    where
        F: Fn(&C) -> Result<T, Error> + Clone + Send + 'static,
        T: Send + 'static,
    {
        const INITIAL_BACKOFF: Duration = Duration::from_millis(100);
        const MAX_BACKOFF: Duration = Duration::from_millis(1500);

        let num_clients = self.client_count();
        let mut errors = Vec::new();

        // Try all electrum clients at least once, or min_retries (whichever is higher)
        let allowed_retries = std::cmp::max(self.config.min_retries, num_clients);
        let mut backoff = INITIAL_BACKOFF;

        while errors.len() < allowed_retries {
            // Back off only once a full pass over all servers has failed
            if !errors.is_empty() && errors.len() % num_clients == 0 {
                trace!(
                    backoff_duration_ms = backoff.as_millis(),
                    "All servers failed in this pass, backing off before the next pass"
                );
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }

            // Every pass starts at the primary (first) server
            let idx = errors.len() % num_clients;

            let client = match self.get_or_init_client_sync(idx) {
                Ok(client) => client,
                Err(err) => {
                    trace!(
                        server_url = self.urls[idx],
                        attempt = errors.len(),
                        error = ?err,
                        "Client initialization failed, switching to next client"
                    );

                    errors.push(err);
                    continue;
                }
            };

            let start = Instant::now();
            match run_with_deadline(client.clone(), f.clone(), self.call_deadline()) {
                Ok(res) => {
                    trace!(
                        server_url = self.urls[idx],
                        attempt = errors.len(),
                        duration_ms = start.elapsed().as_millis(),
                        "Electrum operation successful"
                    );
                    return Ok(res);
                }
                Err(err) => {
                    if is_timeout(&err) {
                        warn!(
                            server_url = self.urls[idx],
                            deadline_seconds = self.config.call_timeout,
                            "Electrum call exceeded the wall-clock deadline; discarding the session"
                        );
                        self.mark_client_hung(idx);
                    } else if is_dead_session_error(&err) {
                        warn!(
                            server_url = self.urls[idx],
                            error = ?err,
                            "Electrum session is dead; discarding it so the next attempt reconnects"
                        );
                        self.reset_client(idx);
                    }
                    trace!(
                        server_url = self.urls[idx],
                        attempt = errors.len(),
                        duration_ms = start.elapsed().as_millis(),
                        error = ?err,
                        "Electrum operation failed, switching to next client"
                    );

                    errors.push(err);
                }
            }
        }

        warn!(
            operation = kind,
            attempts = errors.len(),
            total_attempts = allowed_retries,
            total_clients = self.client_count(),
            error_count = errors.len(),
            all_errors = ?errors,
            "All Electrum clients failed after exhausting retry attempts with backoff"
        );

        let context = format!(
            "All {} Electrum clients failed after {} attempts for operation '{}'",
            self.client_count(),
            errors.len(),
            kind
        );

        Err(MultiError::new(errors, context))
    }

    /// Execute the given closure on all Electrum nodes in parallel, returning
    /// as soon as `min_parallel_responses` nodes have responded successfully.
    ///
    /// The requests to the remaining nodes keep running in the background;
    /// their results are discarded. If the quorum cannot be reached the call
    /// returns once every node has responded.
    ///
    /// The returned results are in completion order, not node order. The
    /// caller is responsible for judging whether the collected responses are
    /// sufficient evidence to act on.
    #[instrument(level = "debug", skip(self, f), fields(operation = kind, total_clients = self.client_count(), min_parallel_responses = self.config.min_parallel_responses))]
    pub async fn join_quorum<F, T>(&self, kind: &str, f: F) -> Result<Vec<Result<T, Error>>, Error>
    where
        F: Fn(&C) -> Result<T, Error> + Send + Sync + Clone + 'static,
        T: Send + 'static,
    {
        self.join_quorum_inner(f, false).await
    }

    /// Like `join_quorum`, but wait for every started request to finish and
    /// return all of their results. Used when the caller must retain a
    /// verification worker slot until its RPC work has stopped.
    #[instrument(level = "debug", skip(self, f), fields(operation = kind, total_clients = self.client_count(), min_parallel_responses = self.config.min_parallel_responses))]
    pub async fn join_quorum_and_drain<F, T>(
        &self,
        kind: &str,
        f: F,
    ) -> Result<Vec<Result<T, Error>>, Error>
    where
        F: Fn(&C) -> Result<T, Error> + Send + Sync + Clone + 'static,
        T: Send + 'static,
    {
        self.join_quorum_inner(f, true).await
    }

    async fn join_quorum_inner<F, T>(
        &self,
        f: F,
        drain: bool,
    ) -> Result<Vec<Result<T, Error>>, Error>
    where
        F: Fn(&C) -> Result<T, Error> + Send + Sync + Clone + 'static,
        T: Send + 'static,
    {
        let start_time = Instant::now();
        let num_clients = self.client_count();
        let quorum = self.config.min_parallel_responses.clamp(1, num_clients);

        let mut tasks: FuturesUnordered<_> = (0..num_clients)
            .map(|idx| {
                let f = f.clone();
                let balancer = self.clone();

                tokio::spawn(async move {
                    match balancer.get_or_init_client_async(idx).await {
                        Ok(client) => {
                            let deadline = balancer.call_deadline();
                            let started = Instant::now();
                            let result = tokio::task::spawn_blocking(move || {
                                run_with_deadline(client, f, deadline)
                            })
                            .await
                            .map_err(|e| Error::IOError(std::io::Error::other(format!("{e:?}"))))
                            .and_then(|result| result);
                            // A quorum call is as slow as its slowest endpoint; say which one
                            // that was, because the pool logged nothing at info level while a
                            // wallet sync took 118 s (2026-11-08).
                            if started.elapsed() >= SLOW_ENDPOINT_CALL {
                                warn!(
                                    endpoint_index = idx,
                                    elapsed_ms = started.elapsed().as_millis() as u64,
                                    ok = result.is_ok(),
                                    "Slow Electrum endpoint call"
                                );
                            }
                            if let Err(err) = &result {
                                if is_timeout(err) {
                                    balancer.mark_client_hung(idx);
                                } else if is_dead_session_error(err) {
                                    balancer.reset_client(idx);
                                }
                            }
                            result
                        }
                        Err(e) => Err(e),
                    }
                })
            })
            .collect();

        let mut results: Vec<Result<T, Error>> = Vec::new();
        let mut successes = 0;

        while let Some(joined) = tasks.next().await {
            match joined {
                Ok(result) => {
                    if result.is_ok() {
                        successes += 1;
                    }
                    results.push(result);
                }
                Err(err) if err.is_cancelled() => {
                    // If one task is cancelled, we do not continue.
                    // Most likely our function got cancelled.
                    return Err(Error::IOError(std::io::Error::other("Task cancelled")));
                }
                Err(err) => {
                    trace!(error = ?err, "Failed to join task for parallel request");
                }
            }

            if successes >= quorum {
                if drain {
                    while let Some(joined) = tasks.next().await {
                        match joined {
                            Ok(result) => results.push(result),
                            Err(error) => {
                                trace!(%error, "Failed to join remaining Electrum request");
                            }
                        }
                    }
                }
                break;
            }
        }

        trace!(
            total_duration_ms = start_time.elapsed().as_millis(),
            successful_requests = successes,
            collected_requests = results.len(),
            detached_requests = tasks.len(),
            "Quorum execution completed"
        );

        Ok(results)
    }

    /// Broadcast the given transaction to all Electrum nodes in parallel.
    ///
    /// The transaction is sent to every node, but the method already returns
    /// once `min_parallel_responses` nodes have accepted it. The remaining
    /// broadcasts continue in the background. Errors for individual nodes do
    /// not abort the others.
    #[instrument(level = "debug", skip(self, tx), fields(txid = %tx.compute_txid(), total_clients = self.client_count()))]
    pub async fn broadcast_all(
        &self,
        tx: Transaction,
    ) -> Result<Vec<Result<bitcoin::Txid, Error>>, Error> {
        let txid = tx.compute_txid();
        let start_time = Instant::now();

        debug!(
            txid = %txid,
            total_clients = self.client_count(),
            "Broadcasting transaction to electrum clients"
        );

        let results = self
            .join_quorum("transaction_broadcast", move |client| {
                client.transaction_broadcast(&tx)
            })
            .await?;

        let success_count = results.iter().filter(|r| r.is_ok()).count();

        if success_count > 0 {
            debug!(
                txid = %txid,
                successful_broadcasts = success_count,
                total_attempts = results.len(),
                duration_ms = start_time.elapsed().as_millis(),
                "Transaction broadcast completed successfully"
            );
        } else {
            error!(
                txid = %txid,
                total_attempts = results.len(),
                duration_ms = start_time.elapsed().as_millis(),
                "Transaction broadcast failed on all servers"
            );
        }

        Ok(results)
    }

    /// Get the URLs used by this balancer
    pub fn urls(&self) -> &Vec<String> {
        &self.urls
    }

    /// Get the current configuration
    pub fn config(&self) -> &ElectrumBalancerConfig {
        &self.config
    }

    /// Populate the transaction cache for all initialized clients.
    pub fn populate_tx_cache(&self, txs: impl IntoIterator<Item = impl Into<Arc<Transaction>>>) {
        // Convert transactions to Arc<Transaction> and collect them since we'll use them for each client
        let transactions: Vec<Arc<Transaction>> = txs.into_iter().map(|tx| tx.into()).collect();
        let clients = self.clients.read().expect("rwlock poisoned");

        let mut initialized_count = 0;

        // Only populate cache for already initialized clients
        for client_once_cell in clients.iter() {
            if let Some(client) = client_once_cell.get() {
                client.populate_tx_cache(transactions.iter().cloned());
                initialized_count += 1;
            }
        }

        trace!(
            transaction_count = transactions.len(),
            initialized_client_count = initialized_count,
            total_client_count = clients.len(),
            "Populated transaction cache for initialized clients"
        );
    }
}

impl<C> Clone for ElectrumBalancer<C>
where
    C: ElectrumClientLike,
{
    fn clone(&self) -> Self {
        Self {
            urls: self.urls.clone(),
            clients: self.clients.clone(),
            hung_at: self.hung_at.clone(),
            config: self.config.clone(),
            factory: self.factory.clone(),
        }
    }
}

/// Trait abstracting Electrum client operations needed by the balancer
pub trait ElectrumClientLike: Send + Sync + 'static {
    /// Broadcast a transaction
    fn transaction_broadcast(&self, tx: &Transaction) -> Result<bitcoin::Txid, Error>;

    /// Populate transaction cache (only for BdkElectrumClient)
    fn populate_tx_cache(&self, _txs: impl Iterator<Item = Arc<Transaction>>) {
        // Default implementation does nothing
    }
}

impl ElectrumClientLike for BdkElectrumClient<Client> {
    fn transaction_broadcast(&self, tx: &Transaction) -> Result<bitcoin::Txid, Error> {
        self.inner.transaction_broadcast(tx)
    }

    fn populate_tx_cache(&self, txs: impl Iterator<Item = Arc<Transaction>>) {
        BdkElectrumClient::populate_tx_cache(self, txs)
    }
}

/// Configuration for the Electrum balancer
#[derive(Clone, Debug)]
pub struct ElectrumBalancerConfig {
    /// Timeout for individual requests in seconds
    pub request_timeout: u8,
    /// Wall-clock cap in seconds for one balancer attempt (the whole
    /// closure, e.g. a full `bdk_electrum` sync chunk).
    ///
    /// The inner socket timeout cannot interrupt a blocking wait on the
    /// client's response channel: when the poller thread dies the caller
    /// waits forever. This deadline abandons the attempt, discards the
    /// session, and lets the balancer fail over to the next server.
    pub call_timeout: u8,
    /// Minimum number of retry attempts across all nodes
    pub min_retries: usize,
    /// Optional SOCKS5 proxy (for Tor onion Electrum endpoints).
    pub socks5_proxy: Option<String>,
    /// Number of successful responses parallel multi-node operations
    /// ([`ElectrumBalancer::join_quorum`], [`ElectrumBalancer::broadcast_all`])
    /// wait for before returning early. Clamped to the number of nodes.
    pub min_parallel_responses: usize,
}

impl Default for ElectrumBalancerConfig {
    fn default() -> Self {
        Self {
            request_timeout: 15,
            call_timeout: 60,
            min_retries: 5,
            socks5_proxy: None,
            min_parallel_responses: 2,
        }
    }
}

/// Trait for creating Electrum clients
pub trait ElectrumClientFactory<C> {
    fn create_client(&self, url: &str, config: &ElectrumBalancerConfig) -> Result<Arc<C>, Error>;
}

fn electrum_client_config(config: &ElectrumBalancerConfig) -> Config {
    let builder = ConfigBuilder::new()
        .timeout(Some(config.request_timeout))
        // Retries are handled by the balancer, not the inner client.
        // Setting this to 0 triggers rust-electrum-client#186.
        .retry(1);

    let builder = match config.socks5_proxy.as_ref() {
        Some(proxy) => builder.socks5(Some(Socks5Config::new(proxy))),
        None => builder,
    };

    builder.build()
}

/// Default factory for BdkElectrumClient
pub struct BdkElectrumClientFactory;

impl ElectrumClientFactory<BdkElectrumClient<Client>> for BdkElectrumClientFactory {
    fn create_client(
        &self,
        url: &str,
        config: &ElectrumBalancerConfig,
    ) -> Result<Arc<BdkElectrumClient<Client>>, Error> {
        let client = Client::from_config(url, electrum_client_config(config)).map_err(|e| {
            // Wrap connection errors with DNS resolution context
            match &e {
                Error::IOError(io_err) if io_err.kind() == std::io::ErrorKind::NotFound => {
                    Error::IOError(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("{e} (Most likely DNS resolution error)"),
                    ))
                }
                Error::IOError(io_err)
                    if io_err.kind() == std::io::ErrorKind::TimedOut
                        || io_err.kind() == std::io::ErrorKind::ConnectionRefused
                        || io_err.kind() == std::io::ErrorKind::ConnectionAborted
                        || io_err.kind() == std::io::ErrorKind::Other =>
                {
                    Error::IOError(std::io::Error::new(
                        io_err.kind(),
                        format!("{e} (Most likely DNS resolution error)"),
                    ))
                }
                _ => e, // Pass through other errors unchanged
            }
        })?;
        let bdk_client = BdkElectrumClient::new(client);

        Ok(Arc::new(bdk_client))
    }
}

// Convenience methods for the default BdkElectrumClient case
impl ElectrumBalancer<BdkElectrumClient<Client>> {
    /// Create a new balancer from a list of Electrum URLs with default configuration.
    /// Uses the default BdkElectrumClientFactory.
    pub async fn new(urls: Vec<String>) -> Result<Self, Error> {
        Self::new_with_factory(urls, Arc::new(BdkElectrumClientFactory)).await
    }

    /// Create a new balancer from a list of Electrum URLs with custom configuration.
    /// Uses the default BdkElectrumClientFactory.
    pub async fn new_with_config(
        urls: Vec<String>,
        config: ElectrumBalancerConfig,
    ) -> Result<Self, Error> {
        Self::new_with_config_and_factory(urls, config, Arc::new(BdkElectrumClientFactory)).await
    }
}

/// Type alias for the default Electrum balancer using BdkElectrumClient
pub type DefaultElectrumBalancer = ElectrumBalancer<BdkElectrumClient<Client>>;

/// Error type that contains multiple Electrum errors from different nodes.
///
/// This allows the caller to inspect all individual failures while still
/// working with the `?` operator through automatic conversion to a single Error.
#[derive(Debug)]
pub struct MultiError {
    pub errors: Vec<Error>,
    pub context: String,
}

impl Clone for MultiError {
    fn clone(&self) -> Self {
        // Clone by converting each error to a string and back to an error
        let cloned_errors = self
            .errors
            .iter()
            .map(|e| Error::IOError(std::io::Error::other(e.to_string())))
            .collect();

        Self {
            errors: cloned_errors,
            context: self.context.clone(),
        }
    }
}

impl MultiError {
    pub fn new(errors: Vec<Error>, context: impl Into<String>) -> Self {
        Self {
            errors,
            context: context.into(),
        }
    }

    /// Get the number of errors
    pub fn len(&self) -> usize {
        self.errors.len()
    }

    /// Check if there are no errors
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// Get an iterator over the errors
    pub fn iter(&self) -> impl Iterator<Item = &Error> {
        self.errors.iter()
    }

    /// Check if any error matches a predicate
    pub fn any<F>(&self, predicate: F) -> bool
    where
        F: Fn(&Error) -> bool,
    {
        self.errors.iter().any(predicate)
    }

    /// Check if all errors match a predicate
    pub fn all<F>(&self, predicate: F) -> bool
    where
        F: Fn(&Error) -> bool,
    {
        self.errors.iter().all(predicate)
    }

    /// Convert to a single Error (uses the last error, or creates a generic one)
    pub fn into_single_error(self) -> Error {
        self.errors.into_iter().next_back().unwrap_or_else(|| {
            Error::IOError(std::io::Error::other(format!(
                "All operations failed: {}",
                self.context
            )))
        })
    }
}

impl std::fmt::Display for MultiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {} errors occurred", self.context, self.errors.len())?;
        for (i, error) in self.errors.iter().enumerate() {
            write!(f, "\n  {}: {}", i + 1, error)?;
        }
        Ok(())
    }
}

impl std::error::Error for MultiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // Return the last error as the source
        self.errors.last().and_then(|e| e.source())
    }
}

impl From<MultiError> for Error {
    fn from(multi_error: MultiError) -> Self {
        multi_error.into_single_error()
    }
}

// Allow ? operator to work on MultiError by converting to Error
impl<T> From<MultiError> for Result<T, Error> {
    fn from(multi_error: MultiError) -> Self {
        Err(multi_error.into())
    }
}

#[cfg(test)]
mod tests {
    use super::electrum_client_config;

    #[test]
    fn socks5_proxy_is_applied_to_electrum_config() {
        let config = super::ElectrumBalancerConfig {
            socks5_proxy: Some("127.0.0.1:9050".to_string()),
            ..Default::default()
        };
        assert_eq!(
            electrum_client_config(&config)
                .socks5()
                .as_ref()
                .map(|socks| socks.addr.as_str()),
            Some("127.0.0.1:9050")
        );
    }

    use super::*;
    use bitcoin::hashes::Hash;
    use bitcoin::{
        Amount, OutPoint, ScriptBuf, Sequence, TxIn, TxOut, Witness, absolute::LockTime,
        transaction::Version,
    };
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock client for testing
    #[derive(Debug)]
    struct MockElectrumClient {
        url: String,
        fail_count: Arc<AtomicUsize>,
        call_count: Arc<AtomicUsize>,
        should_fail: bool,
        error_type: MockErrorType,
        delay: Option<Duration>,
    }

    #[derive(Debug, Clone)]
    enum MockErrorType {
        IOError,
        NonRetryable,
    }

    impl MockElectrumClient {
        fn new(url: String) -> Self {
            Self {
                url,
                fail_count: Arc::new(AtomicUsize::new(0)),
                call_count: Arc::new(AtomicUsize::new(0)),
                should_fail: false,
                error_type: MockErrorType::IOError,
                delay: None,
            }
        }

        fn with_failure(mut self, error_type: MockErrorType) -> Self {
            self.should_fail = true;
            self.error_type = error_type;
            self
        }

        fn with_delay(mut self, delay: Duration) -> Self {
            self.delay = Some(delay);
            self
        }

        fn call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    impl ElectrumClientLike for MockElectrumClient {
        fn transaction_broadcast(&self, _tx: &Transaction) -> Result<bitcoin::Txid, Error> {
            self.call_count.fetch_add(1, Ordering::SeqCst);

            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }

            if self.should_fail {
                self.fail_count.fetch_add(1, Ordering::SeqCst);
                match self.error_type {
                    MockErrorType::IOError => Err(Error::IOError(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        format!("Mock connection failed for {}", self.url),
                    ))),
                    MockErrorType::NonRetryable => Err(Error::Protocol(
                        format!(
                            "\"code\": Number(-5) - transaction not found on {}",
                            self.url
                        )
                        .into(),
                    )),
                }
            } else {
                Ok(bitcoin::Txid::from_raw_hash(
                    bitcoin::hashes::sha256d::Hash::from_byte_array([1; 32]),
                ))
            }
        }
    }

    /// Mock factory for creating test clients
    struct MockElectrumClientFactory {
        clients: Arc<StdMutex<Vec<Arc<MockElectrumClient>>>>,
    }

    impl MockElectrumClientFactory {
        fn new() -> Self {
            Self {
                clients: Arc::new(StdMutex::new(Vec::new())),
            }
        }

        fn add_client(&self, client: MockElectrumClient) {
            self.clients.lock().unwrap().push(Arc::new(client));
        }

        fn get_client(&self, idx: usize) -> Option<Arc<MockElectrumClient>> {
            self.clients.lock().unwrap().get(idx).cloned()
        }
    }

    impl ElectrumClientFactory<MockElectrumClient> for MockElectrumClientFactory {
        fn create_client(
            &self,
            url: &str,
            _config: &ElectrumBalancerConfig,
        ) -> Result<Arc<MockElectrumClient>, Error> {
            let clients = self.clients.lock().unwrap();
            for client in clients.iter() {
                if client.url == url {
                    return Ok(client.clone());
                }
            }

            // If no pre-configured client found, create a default one
            Ok(Arc::new(MockElectrumClient::new(url.to_string())))
        }
    }

    fn create_dummy_transaction() -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(1000),
                script_pubkey: ScriptBuf::new(),
            }],
        }
    }

    #[tokio::test]
    async fn test_balancer_creation() {
        let urls = vec![
            "tcp://localhost:50001".to_string(),
            "tcp://localhost:50002".to_string(),
        ];

        let factory = Arc::new(MockElectrumClientFactory::new());
        let balancer = ElectrumBalancer::new_with_factory(urls.clone(), factory).await;

        assert!(balancer.is_ok());
        let balancer = balancer.unwrap();
        assert_eq!(balancer.client_count(), 2);
        assert_eq!(balancer.urls(), &urls);
    }

    #[tokio::test]
    async fn test_balancer_empty_urls() {
        let factory = Arc::new(MockElectrumClientFactory::new());
        let balancer = ElectrumBalancer::new_with_factory(vec![], factory).await;

        assert!(balancer.is_err());
        match balancer {
            Err(e) => assert!(e.to_string().contains("No Electrum URLs provided")),
            Ok(_) => panic!("Expected error but got Ok"),
        }
    }

    #[tokio::test]
    async fn test_call_always_starts_with_primary() {
        let urls = vec![
            "tcp://localhost:50001".to_string(),
            "tcp://localhost:50002".to_string(),
            "tcp://localhost:50003".to_string(),
        ];

        let factory = Arc::new(MockElectrumClientFactory::new());
        for url in &urls {
            factory.add_client(MockElectrumClient::new(url.clone()));
        }

        let balancer = ElectrumBalancer::new_with_factory(urls, factory.clone())
            .await
            .unwrap();

        // Make several successful calls and verify they all hit the primary
        for _ in 0..6 {
            let result = balancer
                .call("test", |client| {
                    client.transaction_broadcast(&create_dummy_transaction())
                })
                .await;

            assert!(result.is_ok());
        }

        // Verify only the first client was used
        assert_eq!(factory.get_client(0).unwrap().call_count(), 6);
        assert_eq!(factory.get_client(1).unwrap().call_count(), 0);
        assert_eq!(factory.get_client(2).unwrap().call_count(), 0);
    }

    #[tokio::test]
    async fn test_call_switches_on_failure() {
        let urls = vec![
            "tcp://localhost:50001".to_string(),
            "tcp://localhost:50002".to_string(),
            "tcp://localhost:50003".to_string(),
        ];

        let factory = Arc::new(MockElectrumClientFactory::new());
        // First client fails, second succeeds, third not used
        factory.add_client(
            MockElectrumClient::new(urls[0].clone()).with_failure(MockErrorType::IOError),
        );
        factory.add_client(MockElectrumClient::new(urls[1].clone()));
        factory.add_client(MockElectrumClient::new(urls[2].clone()));

        // Use config with min_retries = 0 to test basic switching behavior
        // This ensures total_attempts = max(0, 3) = 3, but behavior is cleaner
        let config = ElectrumBalancerConfig {
            request_timeout: 5,
            call_timeout: 60,
            min_retries: 0,
            socks5_proxy: None,
            min_parallel_responses: 2,
        };

        let balancer = ElectrumBalancer::new_with_config_and_factory(urls, config, factory.clone())
            .await
            .unwrap();

        // First call should try client 0 (fails), then client 1 (succeeds)
        let result1 = balancer
            .call("test", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await;
        assert!(result1.is_ok());

        // The second call starts at the primary again
        let result2 = balancer
            .call("test", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await;
        assert!(result2.is_ok());

        assert_eq!(factory.get_client(0).unwrap().call_count(), 2); // Tried first on both calls
        assert_eq!(factory.get_client(1).unwrap().call_count(), 2); // Used by both calls after client 0 fails
        assert_eq!(factory.get_client(2).unwrap().call_count(), 0); // Never called
    }

    #[tokio::test]
    async fn test_call_with_failing_client() {
        let urls = vec![
            "tcp://localhost:50001".to_string(),
            "tcp://localhost:50002".to_string(),
        ];

        let factory = Arc::new(MockElectrumClientFactory::new());
        // First client fails, second succeeds
        factory.add_client(
            MockElectrumClient::new(urls[0].clone()).with_failure(MockErrorType::IOError),
        );
        factory.add_client(MockElectrumClient::new(urls[1].clone()));

        let balancer = ElectrumBalancer::new_with_factory(urls, factory.clone())
            .await
            .unwrap();

        let result = balancer
            .call("test", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await;

        assert!(result.is_ok());

        // Verify the failing client was called once and the successful client was called once
        assert_eq!(factory.get_client(0).unwrap().call_count(), 1);
        assert_eq!(factory.get_client(1).unwrap().call_count(), 1);
    }

    #[tokio::test]
    async fn test_call_with_non_retryable_error() {
        let urls = vec!["tcp://localhost:50001".to_string()];

        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(
            MockElectrumClient::new(urls[0].clone()).with_failure(MockErrorType::NonRetryable),
        );

        // min_retries = 1 limits the call to a single attempt
        let config = ElectrumBalancerConfig {
            request_timeout: 5,
            call_timeout: 60,
            min_retries: 1,
            socks5_proxy: None,
            min_parallel_responses: 2,
        };

        let balancer = ElectrumBalancer::new_with_config_and_factory(urls, config, factory.clone())
            .await
            .unwrap();

        let result = balancer
            .call("test", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await;

        assert!(result.is_err());
        match result {
            Err(e) => assert!(e.to_string().contains("transaction not found")),
            Ok(_) => panic!("Expected error but got Ok"),
        }

        // Should only be called once (no retry for non-retryable errors)
        assert_eq!(factory.get_client(0).unwrap().call_count(), 1);
    }

    #[tokio::test]
    async fn test_call_all_clients_fail() {
        let urls = vec![
            "tcp://localhost:50001".to_string(),
            "tcp://localhost:50002".to_string(),
        ];

        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(
            MockElectrumClient::new(urls[0].clone()).with_failure(MockErrorType::IOError),
        );
        factory.add_client(
            MockElectrumClient::new(urls[1].clone()).with_failure(MockErrorType::IOError),
        );

        let balancer = ElectrumBalancer::new_with_factory(urls, factory.clone())
            .await
            .unwrap();

        let result = balancer
            .call("test", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await;

        assert!(result.is_err());
        match result {
            Err(e) => {
                let error_msg = e.to_string();
                println!("Error message: {error_msg}");
                assert!(
                    error_msg.contains("All Electrum nodes failed")
                        || error_msg.contains("Mock connection failed")
                );
            }
            Ok(_) => panic!("Expected error but got Ok"),
        }

        // Both clients should have been tried multiple times due to min_retries
        assert!(factory.get_client(0).unwrap().call_count() > 1);
        assert!(factory.get_client(1).unwrap().call_count() > 1);
    }

    #[tokio::test]
    async fn test_broadcast_all() {
        let urls = vec![
            "tcp://localhost:50001".to_string(),
            "tcp://localhost:50002".to_string(),
        ];

        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(MockElectrumClient::new(urls[0].clone()));
        factory.add_client(MockElectrumClient::new(urls[1].clone()));

        let balancer = ElectrumBalancer::new_with_factory(urls, factory.clone())
            .await
            .unwrap();

        let tx = create_dummy_transaction();
        let results = balancer.broadcast_all(tx).await;

        assert!(results.is_ok());
        let results = results.unwrap();
        assert_eq!(results.len(), 2);

        // Both should succeed
        assert!(results[0].is_ok());
        assert!(results[1].is_ok());

        // Both clients should have been called
        assert_eq!(factory.get_client(0).unwrap().call_count(), 1);
        assert_eq!(factory.get_client(1).unwrap().call_count(), 1);
    }

    #[tokio::test]
    async fn test_config_and_urls_accessors() {
        let urls = vec!["tcp://localhost:50001".to_string()];
        let config = ElectrumBalancerConfig {
            request_timeout: 15,
            call_timeout: 60,
            min_retries: 7,
            socks5_proxy: None,
            min_parallel_responses: 3,
        };

        let factory = Arc::new(MockElectrumClientFactory::new());
        let balancer =
            ElectrumBalancer::new_with_config_and_factory(urls.clone(), config.clone(), factory)
                .await
                .unwrap();

        assert_eq!(balancer.urls(), &urls);
        assert_eq!(balancer.config().request_timeout, 15);
        assert_eq!(balancer.config().min_retries, 7);
    }

    #[tokio::test]
    async fn test_populate_tx_cache() {
        let urls = vec!["tcp://localhost:50001".to_string()];

        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(MockElectrumClient::new(urls[0].clone()));

        let balancer = ElectrumBalancer::new_with_factory(urls, factory.clone())
            .await
            .unwrap();

        // Initialize the client first
        let _ = balancer.call("test", |client| Ok(client.url.clone())).await;

        // This should not panic (MockElectrumClient has default implementation)
        let txs = vec![create_dummy_transaction()];
        balancer.populate_tx_cache(txs);
    }

    #[tokio::test]
    async fn test_multi_error_functionality() {
        let urls = vec![
            "tcp://localhost:50001".to_string(),
            "tcp://localhost:50002".to_string(),
            "tcp://localhost:50003".to_string(),
        ];

        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(
            MockElectrumClient::new(urls[0].clone()).with_failure(MockErrorType::IOError),
        );
        factory.add_client(
            MockElectrumClient::new(urls[1].clone()).with_failure(MockErrorType::NonRetryable),
        );
        factory.add_client(
            MockElectrumClient::new(urls[2].clone()).with_failure(MockErrorType::IOError),
        );

        let balancer = ElectrumBalancer::new_with_factory(urls, factory.clone())
            .await
            .unwrap();

        // Use call_async_with_multi_error to get the MultiError
        let result = balancer
            .call_async_with_multi_error("test", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await;

        assert!(result.is_err());
        let multi_error = result.unwrap_err();

        // Check that we have multiple errors
        assert!(multi_error.len() > 1);
        assert!(!multi_error.is_empty());

        // Check that we can inspect individual errors
        let error_count = multi_error.errors.len();
        assert!(error_count > 0);

        // Test the `any` method to find specific error types
        let has_non_retryable =
            multi_error.any(|e| e.to_string().contains("transaction not found"));
        assert!(has_non_retryable);

        // Test converting to single error (should work with ?)
        let single_error: Error = multi_error.clone().into();
        assert!(!single_error.to_string().is_empty());

        // Test that the ? operator works
        fn test_question_mark(multi_error: MultiError) -> Result<(), Error> {
            Err(multi_error)?
        }

        let result = test_question_mark(multi_error);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_call_async_with_multi_error() {
        let urls = vec![
            "tcp://localhost:50001".to_string(),
            "tcp://localhost:50002".to_string(),
        ];

        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(
            MockElectrumClient::new(urls[0].clone()).with_failure(MockErrorType::NonRetryable),
        );
        factory.add_client(
            MockElectrumClient::new(urls[1].clone()).with_failure(MockErrorType::IOError),
        );

        let balancer = ElectrumBalancer::new_with_factory(urls, factory.clone())
            .await
            .unwrap();

        let result = balancer
            .call_async_with_multi_error("test", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await;

        assert!(result.is_err());
        let multi_error = result.unwrap_err();

        // Should have multiple errors due to retries (protocol errors fail
        // over to the next server just like I/O errors)
        assert!(multi_error.len() > 2);

        // Check that there are "transaction not found" type errors
        let has_not_found = multi_error.any(|e| e.to_string().contains("transaction not found"));
        assert!(has_not_found);

        // And I/O errors
        let has_io_error = multi_error.any(|e| e.to_string().contains("Mock connection failed"));
        assert!(has_io_error);
    }

    #[tokio::test]
    async fn test_join_quorum_returns_early_with_slow_node() {
        let urls = vec![
            "tcp://localhost:50001".to_string(),
            "tcp://localhost:50002".to_string(),
            "tcp://localhost:50003".to_string(),
        ];

        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(MockElectrumClient::new(urls[0].clone()));
        factory.add_client(MockElectrumClient::new(urls[1].clone()));
        factory.add_client(
            MockElectrumClient::new(urls[2].clone()).with_delay(Duration::from_secs(2)),
        );

        let config = ElectrumBalancerConfig {
            request_timeout: 5,
            call_timeout: 60,
            min_retries: 0,
            socks5_proxy: None,
            min_parallel_responses: 2,
        };

        let balancer = ElectrumBalancer::new_with_config_and_factory(urls, config, factory.clone())
            .await
            .unwrap();

        let start = Instant::now();
        let results = balancer
            .join_quorum("transaction_broadcast", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await
            .unwrap();

        // Two fast successes satisfy the quorum, the slow node is left behind
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.is_ok()));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn test_join_quorum_and_drain_waits_for_remaining_work() {
        let urls = vec![
            "tcp://localhost:50001".to_string(),
            "tcp://localhost:50002".to_string(),
            "tcp://localhost:50003".to_string(),
        ];

        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(MockElectrumClient::new(urls[0].clone()));
        factory.add_client(MockElectrumClient::new(urls[1].clone()));
        factory.add_client(
            MockElectrumClient::new(urls[2].clone()).with_delay(Duration::from_millis(200)),
        );

        let config = ElectrumBalancerConfig {
            request_timeout: 5,
            call_timeout: 60,
            min_retries: 0,
            socks5_proxy: None,
            min_parallel_responses: 2,
        };

        let balancer = ElectrumBalancer::new_with_config_and_factory(urls, config, factory.clone())
            .await
            .unwrap();

        let start = Instant::now();
        let results = balancer
            .join_quorum_and_drain("transaction_broadcast", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await
            .unwrap();

        // Retain the worker until the slow call ends, and keep its result.
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.is_ok()));
        assert!(start.elapsed() >= Duration::from_millis(200));
    }

    #[tokio::test]
    async fn test_join_quorum_does_not_wait_for_primary() {
        let urls = vec![
            "tcp://localhost:50001".to_string(),
            "tcp://localhost:50002".to_string(),
            "tcp://localhost:50003".to_string(),
        ];

        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(
            MockElectrumClient::new(urls[0].clone()).with_delay(Duration::from_millis(500)),
        );
        factory.add_client(MockElectrumClient::new(urls[1].clone()));
        factory.add_client(MockElectrumClient::new(urls[2].clone()));

        let config = ElectrumBalancerConfig {
            request_timeout: 5,
            call_timeout: 60,
            min_retries: 0,
            socks5_proxy: None,
            min_parallel_responses: 2,
        };

        let balancer = ElectrumBalancer::new_with_config_and_factory(urls, config, factory.clone())
            .await
            .unwrap();

        let start = Instant::now();
        let results = balancer
            .join_quorum("transaction_broadcast", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await
            .unwrap();

        // The quorum of 2 is reached by the fast nodes, so the slow primary
        // (first) node is not awaited
        assert_eq!(results.len(), 2);
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn hung_call_times_out_and_single_endpoint_retries_immediately() {
        let url = "mock://hang".to_string();
        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(MockElectrumClient::new(url.clone()).with_delay(Duration::from_secs(30)));
        let hung_client = factory.get_client(0).unwrap();

        let config = ElectrumBalancerConfig {
            request_timeout: 5,
            call_timeout: 1,
            min_retries: 1,
            socks5_proxy: None,
            min_parallel_responses: 1,
        };
        let balancer = ElectrumBalancer::new_with_config_and_factory(
            vec![url],
            config,
            factory,
        )
        .await
        .unwrap();

        let start = Instant::now();
        let err = balancer
            .call("transaction_broadcast", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await
            .unwrap_err();
        assert!(is_timeout(&err), "expected a timeout, got {err:?}");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "call blocked past the deadline: {:?}",
            start.elapsed()
        );

        // With a single endpoint the dead session is dropped and the next call
        // is a real attempt on a fresh session. It must not fail instantly with
        // a "cooling down" error: the taker looped on that for minutes
        // (swap 44c493a4, 2026-11-06).
        let calls_before = hung_client.call_count();
        let start = Instant::now();
        let second = balancer
            .call("transaction_broadcast", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await
            .unwrap_err();
        assert!(is_timeout(&second), "expected a real timeout, got {second:?}");
        assert!(
            hung_client.call_count() > calls_before,
            "the second call never reached the server (it was fast-failed)"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "second call blocked past the deadline: {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn single_endpoint_hang_resets_the_session_without_cooldown() {
        let url = "mock://single".to_string();
        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(MockElectrumClient::new(url.clone()));
        let balancer = ElectrumBalancer::new_with_config_and_factory(
            vec![url],
            ElectrumBalancerConfig {
                call_timeout: 1,
                min_retries: 1,
                min_parallel_responses: 1,
                ..Default::default()
            },
            factory,
        )
        .await
        .unwrap();

        assert!(balancer.get_or_init_client_sync(0).is_ok());
        let before = balancer.clients.read().unwrap()[0].clone();

        balancer.mark_client_hung(0);

        assert!(
            balancer.get_or_init_client_sync(0).is_ok(),
            "a single endpoint must reconnect immediately, not cool down"
        );
        let after = balancer.clients.read().unwrap()[0].clone();
        assert!(!Arc::ptr_eq(&before, &after), "the dead session was not replaced");
        assert_eq!(balancer.hung_at[0].load(Ordering::SeqCst), 0);
    }

    #[test]
    fn dead_session_errors_are_recognised_including_nested_ones() {
        let would_block = || Error::IOError(std::io::Error::from(std::io::ErrorKind::WouldBlock));
        // The exact shape of the swap f7a162f7 failure.
        assert!(is_dead_session_error(&Error::AllAttemptsErrored(vec![would_block()])));
        assert!(is_dead_session_error(&would_block()));
        assert!(is_dead_session_error(&Error::SharedIOError(Arc::new(
            std::io::Error::from(std::io::ErrorKind::BrokenPipe)
        ))));
        // A server rejecting a request, or a refused connect, is not a dead session.
        assert!(!is_dead_session_error(&Error::Protocol("txindex not ready".into())));
        assert!(!is_dead_session_error(&Error::IOError(std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused
        ))));
        assert!(!is_dead_session_error(&Error::AllAttemptsErrored(vec![
            Error::Message("nope".to_string())
        ])));
    }

    #[tokio::test]
    async fn dead_session_error_replaces_the_session_but_a_server_error_does_not() {
        let url = "mock://dead".to_string();
        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(MockElectrumClient::new(url.clone()));
        let balancer = ElectrumBalancer::new_with_config_and_factory(
            vec![url],
            ElectrumBalancerConfig {
                min_retries: 1,
                min_parallel_responses: 1,
                ..Default::default()
            },
            factory,
        )
        .await
        .unwrap();
        assert!(balancer.get_or_init_client_sync(0).is_ok());
        let original = balancer.clients.read().unwrap()[0].clone();

        let _ = balancer
            .call("probe", |_client| -> Result<(), Error> {
                Err(Error::Protocol("txindex not ready".into()))
            })
            .await;
        assert!(
            Arc::ptr_eq(&original, &balancer.clients.read().unwrap()[0]),
            "a server-side error must keep the session"
        );

        let _ = balancer
            .call("probe", |_client| -> Result<(), Error> {
                Err(Error::AllAttemptsErrored(vec![Error::IOError(
                    std::io::Error::from(std::io::ErrorKind::WouldBlock),
                )]))
            })
            .await;
        assert!(
            !Arc::ptr_eq(&original, &balancer.clients.read().unwrap()[0]),
            "a dead-session error (EAGAIN) must replace the session"
        );
    }

    #[tokio::test]
    async fn cooldown_expiry_resets_the_session() {
        // The cooldown only applies when there is another endpoint to use.
        let url = "mock://reset".to_string();
        let other = "mock://reset-other".to_string();
        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(MockElectrumClient::new(url.clone()));
        factory.add_client(MockElectrumClient::new(other.clone()));
        let balancer = ElectrumBalancer::new_with_config_and_factory(
            vec![url, other],
            ElectrumBalancerConfig {
                call_timeout: 1,
                min_retries: 1,
                min_parallel_responses: 1,
                ..Default::default()
            },
            factory,
        )
        .await
        .unwrap();

        assert!(balancer.get_or_init_client_sync(0).is_ok());

        balancer.mark_client_hung(0);
        let err = balancer.get_or_init_client_sync(0).unwrap_err();
        match err {
            Error::IOError(e) => assert_ne!(
                e.kind(),
                std::io::ErrorKind::TimedOut,
                "the cooldown fast-fail must not look like a deadline expiry"
            ),
            other => panic!("unexpected error: {other:?}"),
        }

        // Simulate the cooldown elapsing: the dead session is dropped and a
        // fresh one is created, so a recovered network heals without a restart.
        balancer.hung_at[0].store(
            now_millis() - HUNG_CLIENT_COOLDOWN.as_millis() as u64 - 1,
            Ordering::SeqCst,
        );
        assert!(balancer.get_or_init_client_sync(0).is_ok());
        assert_eq!(balancer.hung_at[0].load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn hung_primary_fails_over_to_healthy_secondary() {
        let primary = "mock://hang-primary".to_string();
        let secondary = "mock://healthy-secondary".to_string();
        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(
            MockElectrumClient::new(primary.clone()).with_delay(Duration::from_secs(30)),
        );
        factory.add_client(MockElectrumClient::new(secondary.clone()));

        let balancer = ElectrumBalancer::new_with_config_and_factory(
            vec![primary, secondary],
            ElectrumBalancerConfig {
                call_timeout: 1,
                min_retries: 2,
                min_parallel_responses: 1,
                ..Default::default()
            },
            factory.clone(),
        )
        .await
        .unwrap();

        let start = Instant::now();
        let result = balancer
            .call("transaction_broadcast", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await;
        assert!(
            result.is_ok(),
            "the healthy secondary must serve after the primary hangs: {result:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "failover took {:?}",
            start.elapsed()
        );
        assert_eq!(
            factory.get_client(1).unwrap().call_count(),
            1,
            "the secondary was never used"
        );
    }

    #[tokio::test]
    async fn slow_call_within_the_deadline_is_not_aborted() {
        let url = "mock://slow".to_string();
        let factory = Arc::new(MockElectrumClientFactory::new());
        factory.add_client(MockElectrumClient::new(url.clone()).with_delay(Duration::from_millis(300)));

        let balancer = ElectrumBalancer::new_with_config_and_factory(
            vec![url],
            ElectrumBalancerConfig {
                call_timeout: 5,
                min_retries: 1,
                min_parallel_responses: 1,
                ..Default::default()
            },
            factory,
        )
        .await
        .unwrap();

        let start = Instant::now();
        let result = balancer
            .call("transaction_broadcast", |client| {
                client.transaction_broadcast(&create_dummy_transaction())
            })
            .await;
        assert!(result.is_ok(), "slow-but-legal call was aborted: {result:?}");
        assert!(start.elapsed() >= Duration::from_millis(300));
    }
}
