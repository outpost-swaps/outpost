use crate::monero::distribution_cache;
use crate::monero::{Amount, PrivateViewKey, TxHash};
use ::monero::{Address, Network as MoneroNetwork, PrivateKey, PublicKey};
use anyhow::{Context as _, Result};
use monero_daemon_rpc::{HttpTransport, MoneroDaemon};
use monero_interface::{InterfaceError, ProvidesBlockchainMeta as _, PublishTransaction as _};
use monero_oxide::ed25519::{CompressedPoint, Point, Scalar};
use monero_oxide::transaction::{NotPruned, Transaction};
use monero_oxide_wallet::address::{MoneroAddress, Network};
use std::fmt::Debug;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use zeroize::Zeroizing;

/// A daemon connection used by the key-derived (non-wallet-rpc) operations.
///
/// The transport is a plain reqwest client so it can reuse Outpost's SOCKS
/// proxy configuration and keep daemon traffic on the same privacy path as
/// wallet-rpc. Local daemon URLs (such as Outpost's daemon pool proxy) are
/// deliberately reached directly: proxying `127.0.0.1` through Tor would ask
/// the exit node to connect to its own loopback instead of this machine.
#[derive(Clone)]
pub struct Daemon {
    inner: MoneroDaemon<RpcTransport>,
    distribution: Arc<distribution_cache::DistributionCache>,
    /// Direct REST client for endpoints wallet-RPC needs but the daemon's
    /// JSON-RPC surface does not expose (`/get_transactions`).
    rest_client: reqwest::Client,
    rest_base_url: reqwest::Url,
    rest_auth: Option<(String, String)>,
    /// Other nodes that must agree with this one about Alice's lock (see `witness`).
    witnesses: Arc<std::sync::RwLock<Option<super::witness::Witnesses>>>,
    /// Set when the policy needs witnesses: without any configured, the lock check must fail
    /// rather than pass (see `witness`).
    witnesses_required: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone)]
pub struct RpcTransport {
    client: reqwest::Client,
    base_url: reqwest::Url,
    auth: Option<(String, String)>,
    distribution: Arc<distribution_cache::DistributionCache>,
}

impl Debug for RpcTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcTransport")
            .field("base_url", &self.base_url)
            .field("auth", &self.auth.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

const DAEMON_POST_ATTEMPTS: u32 = 4;

/// Whether a failed daemon request may simply be sent again. Everything is a
/// read except transaction submission.
fn daemon_post_is_retryable(route: &str) -> bool {
    !route.contains("send_raw_transaction") && !route.contains("submit")
}

/// Replies taking this long, or at least this many bytes, are logged with their size.
const SLOW_DAEMON_REPLY: Duration = Duration::from_secs(5);
const LARGE_DAEMON_REPLY: usize = 256 * 1024;

fn daemon_post_retry_delay(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << attempt.saturating_sub(1).min(3))
}

#[cfg(test)]
mod daemon_post_retry_tests {
    use super::*;

    #[test]
    fn reads_are_retried_but_transaction_submission_is_not() {
        assert!(daemon_post_is_retryable("get_outs"));
        assert!(daemon_post_is_retryable("json_rpc"));
        assert!(daemon_post_is_retryable("get_transactions"));
        assert!(!daemon_post_is_retryable("send_raw_transaction"));
        assert!(!daemon_post_is_retryable("submit_block"));
    }

    #[test]
    fn the_delay_doubles_from_one_second() {
        assert_eq!(daemon_post_retry_delay(1), Duration::from_secs(1));
        assert_eq!(daemon_post_retry_delay(2), Duration::from_secs(2));
        assert_eq!(daemon_post_retry_delay(3), Duration::from_secs(4));
    }
}

impl HttpTransport for RpcTransport {
    fn post(
        &self,
        route: &str,
        body: Vec<u8>,
        response_size_limit: Option<usize>,
    ) -> impl Send + std::future::Future<Output = Result<Vec<u8>, InterfaceError>> {
        let this = self.clone();
        let route = route.to_string();
        async move {
            if route == distribution_cache::ROUTE {
                // The whole-chain output distribution is ~17 MB; keep a copy and
                // fetch only the tail (see `distribution_cache`).
                let sender = this.clone();
                return distribution_cache::distribution_post(
                    &this.distribution,
                    body,
                    move |body| {
                        let sender = sender.clone();
                        async move {
                            sender
                                .post_uncached(distribution_cache::ROUTE, body, response_size_limit)
                                .await
                        }
                    },
                )
                .await;
            }
            this.post_uncached(&route, body, response_size_limit).await
        }
    }
}

impl RpcTransport {
    fn post_uncached(
        &self,
        route: &str,
        body: Vec<u8>,
        response_size_limit: Option<usize>,
    ) -> impl Send + std::future::Future<Output = Result<Vec<u8>, InterfaceError>> {
        let client = self.client.clone();
        let url = self.base_url.join(route).map_err(|error| {
            InterfaceError::InterfaceError(format!("invalid daemon route {route}: {error}"))
        });
        let auth = self.auth.clone();

        let route = route.to_string();
        async move {
            let url = url?;
            let is_batch = body.first() == Some(&b'[');
            // Read-only calls are retried a few times: a connection dropped mid
            // response (reqwest: "error decoding response body") used to end a
            // whole sweep construction made of dozens of calls. Transaction
            // submission is never retried here.
            let mut attempt = 1u32;
            let (status, bytes) = loop {
                let started = std::time::Instant::now();
                let mut request = client
                    .post(url.clone())
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.clone());
                if let Some((username, password)) = auth.clone() {
                    request = request.basic_auth(username, Some(password));
                }

                let result: Result<_, InterfaceError> = async {
                    let response = request
                        .send()
                        .await
                        .map_err(|error| InterfaceError::InterfaceError(error.to_string()))?;
                    let status = response.status();
                    let bytes = response
                        .bytes()
                        .await
                        .map_err(|error| InterfaceError::InterfaceError(error.to_string()))?;
                    Ok((status, bytes))
                }
                .await;

                match result {
                    Ok(value) => {
                        // Size and duration of slow or large replies: the output
                        // distribution timed out for weeks of "slow Monero redeem"
                        // reports without anyone knowing how big it is.
                        if started.elapsed() >= SLOW_DAEMON_REPLY || value.1.len() >= LARGE_DAEMON_REPLY {
                            tracing::info!(
                                %route,
                                attempt,
                                elapsed_ms = started.elapsed().as_millis() as u64,
                                bytes = value.1.len(),
                                "Monero daemon reply was slow or large"
                            );
                        }
                        break value;
                    }
                    Err(error)
                        if attempt < DAEMON_POST_ATTEMPTS && daemon_post_is_retryable(&route) =>
                    {
                        let delay = daemon_post_retry_delay(attempt);
                        tracing::warn!(%route, attempt, ?delay, %error, elapsed_ms = started.elapsed().as_millis() as u64, "Monero daemon request failed; retrying");
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    }
                    Err(error) => return Err(error),
                }
            };

            if !status.is_success() {
                // Some public daemons/frontends reject JSON-RPC batch requests
                // with HTTP 501. monero-daemon-rpc probes batch support during
                // initialization and treats a -32700 parse error as "use single
                // requests". Translate that specific failure instead of aborting
                // the whole ASB startup.
                if status == reqwest::StatusCode::NOT_IMPLEMENTED && is_batch {
                    return Ok(br#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"Parse error"},"id":0}"#.to_vec());
                }
                return Err(InterfaceError::InterfaceError(format!(
                    "daemon returned HTTP {status} for {url}"
                )));
            }

            if response_size_limit.is_some_and(|limit| bytes.len() > limit) {
                return Err(InterfaceError::InterfaceError(format!(
                    "daemon response exceeds {} bytes",
                    response_size_limit.expect("checked above")
                )));
            }

            Ok(bytes.to_vec())
        }
    }
}

impl Daemon {
    /// Connect to a Monero daemon.
    ///
    /// `url` may be `host:port` or a full `http(s)://` URL. Authentication is
    /// optional and sent as HTTP basic auth; public Monero nodes do not need it.
    pub async fn connect(
        url: &str,
        proxy: Option<&str>,
        auth: Option<(String, String)>,
    ) -> Result<Self> {
        let base_url = normalize_url(url)?;
        require_tor_for_remote_daemon(&base_url, proxy)?;
        // No total-request timeout: `get_output_distribution.bin` is a large body that
        // takes longer than 60 s over an onion daemon, and a total timeout cut it off
        // every time (swap 577b2b7c, 2026-11-08). A stalled connection still fails:
        // connecting and every read must make progress within the limits below.
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(60))
            .read_timeout(Duration::from_secs(60));
        if let Some(proxy) = proxy {
            if let Some(proxy_url) = daemon_proxy(&base_url, proxy)? {
                builder = builder.proxy(
                    reqwest::Proxy::all(proxy_url)
                        .with_context(|| format!("invalid daemon proxy {proxy}"))?,
                );
            }
        }
        let client = builder
            .build()
            .context("failed to build daemon HTTP client")?;

        let transport = RpcTransport {
            client: client.clone(),
            base_url: base_url.clone(),
            auth: auth.clone(),
            distribution: Arc::new(distribution_cache::DistributionCache::default()),
        };
        let distribution = transport.distribution.clone();
        let inner = MoneroDaemon::new(transport)
            .await
            .context("failed to initialize Monero daemon interface")?;

        // `MoneroDaemon::new` only proves that the transport rejected an
        // unsupported JSON-RPC batch, which our 501 workaround intentionally
        // converts into "batch unsupported". Query a real endpoint here so a
        // misconfigured proxy or unusable daemon stops the ASB at startup,
        // rather than after a swap's Bitcoin lock has already been broadcast.
        let height = inner
            .latest_block_number()
            .await
            .context("failed to read the Monero daemon height during startup")?;
        tracing::info!(height, "Monero daemon interface ready");

        Ok(Self {
            inner,
            distribution,
            rest_client: client,
            rest_base_url: base_url,
            rest_auth: auth,
            witnesses: Arc::default(),
            witnesses_required: Arc::default(),
        })
    }

    /// The nodes whose agreement the lock verification needs besides this one.
    pub fn set_witnesses(&self, witnesses: super::witness::Witnesses) {
        *self.witnesses.write().expect("witness lock") = Some(witnesses);
    }

    /// Whether the policy requires witnesses for this daemon's lock checks.
    pub fn set_witnesses_required(&self, required: bool) {
        self.witnesses_required
            .store(required, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn witnesses_required(&self) -> bool {
        self.witnesses_required
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn witnesses(&self) -> Option<super::witness::Witnesses> {
        self.witnesses.read().expect("witness lock").clone()
    }

    /// Keep the output distribution in `path` so later decoy selections download
    /// only the blocks added since (see `distribution_cache`).
    pub fn enable_distribution_cache(&self, path: std::path::PathBuf) {
        self.distribution.set_path(path);
    }

    /// Download (or top up) the output distribution now, so a redeem later
    /// finds it ready. Failures are only logged: the redeem fetches it itself.
    pub async fn warm_output_distribution(&self) {
        use monero_interface::ProvidesUnvalidatedDecoys as _;
        let started = std::time::Instant::now();
        let tip = match self.inner.latest_block_number().await {
            Ok(tip) => tip,
            Err(error) => {
                tracing::warn!(%error, "Could not read the Monero height to prefetch the output distribution");
                return;
            }
        };
        match self.inner.ringct_output_distribution(..=tip).await {
            Ok(values) => tracing::info!(
                blocks = values.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "V5_BOB_OUTPUT_DISTRIBUTION_READY"
            ),
            Err(error) => tracing::warn!(%error, "Could not prefetch the Monero output distribution"),
        }
    }

    /// Wrap this daemon as the protocol-5 wallet boundary.
    pub fn into_wallets(self) -> outpost_monero_wallet::Wallets {
        outpost_monero_wallet::Wallets::new(Arc::new(self))
    }

    /// Verify that `txid` pays `expected` to the view pair.
    pub async fn verify_transfer(
        &self,
        txid: &TxHash,
        public_spend_key: &PublicKey,
        private_view_key: &PrivateViewKey,
        expected: Amount,
    ) -> Result<bool> {
        let tx_id = decode_txid(txid)?;
        monero_wallet_ng::verify::verify_transfer(
            &self.inner,
            tx_id,
            point(public_spend_key)?,
            scalar(private_view_key.as_bytes())?,
            expected.as_piconero(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("Monero verification failed: {error}"))
    }

    /// Verify a protocol-5 transfer proof using raw key bytes.
    pub async fn verify_protocol_transfer(
        &self,
        tx_hash: &str,
        public_spend_key: &[u8; 32],
        private_view_key: &[u8],
        expected_pico: u64,
    ) -> Result<bool> {
        let public_spend_key = ::monero::PublicKey::from_slice(public_spend_key)
            .map_err(|error| anyhow::anyhow!("invalid protocol public spend key: {error}"))?;
        let private_view_key = super::PrivateViewKey(
            ::monero::PrivateKey::from_slice(private_view_key)
                .map_err(|error| anyhow::anyhow!("invalid protocol private view key: {error}"))?,
        );

        self.verify_transfer(
            &TxHash(tx_hash.to_owned()),
            &public_spend_key,
            &private_view_key,
            Amount::from_piconero(expected_pico),
        )
        .await
    }

    /// Whether `tx_hash` pays the view pair at least `minimum_pico` in a single
    /// output that is spendable now (not timelocked). Wallet-RPC `check_tx_key`
    /// ignores the unlock time and sums all outputs, so the lock must also pass
    /// this before Bob reveals his redeem signature.
    pub async fn has_spendable_lock_output(
        &self,
        tx_hash: &str,
        public_spend_key: &[u8; 32],
        private_view_key: &[u8],
        minimum_pico: u64,
    ) -> Result<bool> {
        let public_spend_key = ::monero::PublicKey::from_slice(public_spend_key)
            .map_err(|error| anyhow::anyhow!("invalid protocol public spend key: {error}"))?;
        let private_view_key = super::PrivateViewKey(
            ::monero::PrivateKey::from_slice(private_view_key)
                .map_err(|error| anyhow::anyhow!("invalid protocol private view key: {error}"))?,
        );
        let tx_id = decode_txid(&TxHash(tx_hash.to_owned()))?;
        monero_wallet_ng::verify::has_spendable_output(
            &self.inner,
            tx_id,
            point(&public_spend_key)?,
            scalar(private_view_key.as_bytes())?,
            minimum_pico,
        )
        .await
        .map_err(|error| anyhow::anyhow!("Monero spendable-output check failed: {error}"))
    }

    /// Wait until the daemon can return the transaction through its REST
    /// `/get_transactions` endpoint.
    ///
    /// The wallet-RPC `check_tx_key` call throws and stops its RPC server when
    /// the daemon has not seen a just-broadcast transaction yet. That endpoint
    /// is safe to poll; once the tx is visible the wallet call can run.
    pub async fn wait_for_tx_visible(&self, txid: &str, timeout: Duration) -> Result<()> {
        let txid = txid.trim();
        let url = self
            .rest_base_url
            .join("get_transactions")
            .with_context(|| format!("invalid daemon REST URL for {txid}"))?;
        let body = get_transactions_body(txid)?;
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let mut request = self
                .rest_client
                .post(url.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.clone());
            if let Some((username, password)) = &self.rest_auth {
                request = request.basic_auth(username, Some(password));
            }

            if let Ok(response) = request.send().await {
                if response.status().is_success() {
                    if let Ok(bytes) = response.bytes().await {
                        if let Ok(value) =
                            serde_json::from_slice::<serde_json::Value>(&bytes)
                        {
                            let visible = value
                                .get("txs")
                                .and_then(|txs| txs.as_array())
                                .is_some_and(|txs| !txs.is_empty());
                            if visible {
                                return Ok(());
                            }
                        }
                    }
                }
            }

            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "Monero transaction {txid} was not visible through the daemon within {}s",
                    timeout.as_secs()
                );
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// Height of the block containing `txid`; `None` while it is in the mempool.
    ///
    /// Used to start the redeem wallet's scan at the XMR lock block instead of
    /// a recorded height that may be thousands of blocks stale.
    pub async fn tx_block_height(&self, txid: &str) -> Result<Option<u64>> {
        let bytes = self.get_transactions_bytes(txid).await?;
        parse_tx_block_height(&bytes).context("daemon returned invalid JSON")
    }

    /// Whether `txid` is mined, waiting in the mempool, or unknown to the daemon.
    pub async fn tx_status(&self, txid: &str) -> Result<TxStatus> {
        let bytes = self.get_transactions_bytes(txid).await?;
        parse_tx_status(&bytes).context("daemon returned invalid JSON")
    }

    async fn get_transactions_bytes(&self, txid: &str) -> Result<Vec<u8>> {
        let txid = txid.trim();
        let url = self
            .rest_base_url
            .join("get_transactions")
            .with_context(|| format!("invalid daemon REST URL for {txid}"))?;
        let body = get_transactions_body(txid)?;
        let mut request = self
            .rest_client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        if let Some((username, password)) = &self.rest_auth {
            request = request.basic_auth(username, Some(password));
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("failed to query the daemon for transaction {txid}"))?;
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("failed to read the daemon response for {txid}"))?;
        Ok(bytes.to_vec())
    }

    /// Current daemon chain tip.
    pub async fn chain_height(&self) -> Result<u64> {
        let height = self
            .inner
            .latest_block_number()
            .await
            .context("failed to read the Monero daemon height")?;
        u64::try_from(height).context("Monero daemon height does not fit u64")
    }

    /// Broadcast a fully-signed raw transaction through the daemon.
    ///
    /// Wallet-RPC does not expose a raw transaction submission method in the
    /// pinned Monero release, so refunds and recoveries built by the key-
    /// derived wallet must go directly to the daemon.
    pub async fn publish_raw_transaction(&self, tx_hex: &str) -> Result<()> {
        let bytes = hex::decode(tx_hex).context("Raw Monero transaction is not valid hex")?;
        let mut reader = bytes.as_slice();
        let transaction = Transaction::<NotPruned>::read(&mut reader)
            .context("Failed to deserialize raw Monero transaction")?;
        if !reader.is_empty() {
            anyhow::bail!("Raw Monero transaction has trailing bytes");
        }
        self.inner
            .publish_transaction(&transaction)
            .await
            .map_err(|error| anyhow::anyhow!("Failed to submit raw Monero transaction: {error}"))
    }

    /// Whether the view pair has received any output from `start_height`
    /// through `target_tip` (or the current tip when `None`), including mempool.
    pub async fn scan_received_outputs(
        &self,
        public_spend_key: &PublicKey,
        private_view_key: &PrivateViewKey,
        start_height: u64,
        target_tip: Option<u64>,
    ) -> Result<bool> {
        monero_wallet_ng::empty::has_received_outputs(
            &self.inner,
            point(public_spend_key)?,
            scalar(private_view_key.as_bytes())?,
            usize::try_from(start_height).context("start height does not fit usize")?,
            target_tip
                .map(usize::try_from)
                .transpose()
                .context("target tip does not fit usize")?,
            None,
        )
        .await
        .map_err(|error| anyhow::anyhow!("Monero output scan failed: {error}"))
    }

    /// Construct a signed sweep of the largest output belonging to the view
    /// pair in `txid` to a single destination.
    pub async fn construct_sweep_to_single(
        &self,
        private_spend_key: &PrivateKey,
        private_view_key: &PrivateViewKey,
        txid: &TxHash,
        destination: &Address,
        network: MoneroNetwork,
    ) -> Result<ConstructedSweep> {
        let tx = monero_wallet_ng::sweep::construct_sweep_tx_to_single(
            self.inner.clone(),
            scalar(private_spend_key.as_bytes())?,
            scalar(private_view_key.as_bytes())?,
            decode_txid(txid)?,
            address(destination, network)?,
            None,
        )
        .await
        .map_err(|error| anyhow::anyhow!("Monero sweep construction failed: {error}"))?;

        Ok(ConstructedSweep::new(tx))
    }

    /// Construct a data-carrying transaction (used for Hermes messages).
    pub async fn construct_data_tx(
        &self,
        private_spend_key: &PrivateKey,
        private_view_key: &PrivateViewKey,
        txid: &TxHash,
        destination: &Address,
        network: MoneroNetwork,
        data: Vec<Vec<u8>>,
    ) -> Result<ConstructedSweep> {
        let tx = monero_wallet_ng::sweep::construct_data_tx(
            self.inner.clone(),
            scalar(private_spend_key.as_bytes())?,
            scalar(private_view_key.as_bytes())?,
            decode_txid(txid)?,
            address(destination, network)?,
            data,
            None,
        )
        .await
        .map_err(|error| anyhow::anyhow!("Monero sweep construction failed: {error}"))?;

        Ok(ConstructedSweep::new(tx))
    }
}

#[derive(Clone, Debug)]
pub struct ConstructedSweep {
    pub tx_hash: TxHash,
    pub tx_hex: String,
}

impl outpost_monero_wallet::HasReceivedOutputs for Daemon {
    fn has_received_outputs<'a>(
        &'a self,
        public_spend_key: [u8; 32],
        private_view_key: [u8; 32],
        start_height: u64,
        target_tip: Option<u64>,
        _inner_retry: Option<backoff::ExponentialBackoff>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<bool, anyhow::Error>> + Send + 'a>> {
        Box::pin(async move {
            let public_spend_key = ::monero::PublicKey::from_slice(&public_spend_key)
                .map_err(|error| anyhow::anyhow!("invalid protocol public spend key: {error}"))?;
            let private_view_key = super::PrivateViewKey(
                ::monero::PrivateKey::from_slice(&private_view_key).map_err(|error| {
                    anyhow::anyhow!("invalid protocol private view key: {error}")
                })?,
            );

            self.scan_received_outputs(
                &public_spend_key,
                &private_view_key,
                start_height,
                target_tip,
            )
            .await
        })
    }
}

impl ConstructedSweep {
    /// The signed transaction again, from its stored wire-format hex.
    pub fn transaction(&self) -> Result<Transaction<NotPruned>> {
        let bytes = hex::decode(&self.tx_hex).context("Stored Monero sweep is not valid hex")?;
        let mut reader = bytes.as_slice();
        let transaction = Transaction::<NotPruned>::read(&mut reader)
            .context("Failed to deserialize the stored Monero sweep")?;
        if !reader.is_empty() {
            anyhow::bail!("Stored Monero sweep has trailing bytes");
        }
        Ok(transaction)
    }

    /// The network fee the sweep pays, in piconero. A RingCT transaction carries its fee in
    /// the clear (`proofs.base.fee`), so a stored sweep gives it too (matrix N-15).
    pub fn fee_piconero(&self) -> Result<u64> {
        match self.transaction()? {
            Transaction::V2 {
                proofs: Some(proofs),
                ..
            } => Ok(proofs.base.fee),
            _ => anyhow::bail!("The Monero sweep is not a RingCT transaction; its fee is unknown"),
        }
    }

    pub fn from_transaction(tx: Transaction<NotPruned>) -> Self {
        Self::new(tx)
    }

    fn new(tx: Transaction<NotPruned>) -> Self {
        Self {
            tx_hash: TxHash(hex::encode(tx.hash())),
            tx_hex: hex::encode(tx.serialize()),
        }
    }
}

/// Where a transaction is according to the daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxStatus {
    /// Not found: never sent, dropped from the pool, or on another chain.
    Unknown,
    /// Accepted by the daemon and waiting for a block.
    InPool,
    /// Included in the block at this height.
    Mined(u64),
}

/// Read a daemon `get_transactions` response for one transaction. `in_pool` wins over
/// `block_height`, because daemons report a placeholder height for pool transactions.
fn parse_tx_status(bytes: &[u8]) -> Result<TxStatus> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let Some(tx) = value
        .get("txs")
        .and_then(|txs| txs.as_array())
        .and_then(|txs| txs.first())
    else {
        return Ok(TxStatus::Unknown);
    };
    if tx.get("in_pool").and_then(|in_pool| in_pool.as_bool()) == Some(true) {
        return Ok(TxStatus::InPool);
    }
    Ok(match tx.get("block_height").and_then(|height| height.as_u64()) {
        Some(height) if height > 0 && height != u64::MAX => TxStatus::Mined(height),
        _ => TxStatus::Unknown,
    })
}

/// Extract `txs[0].block_height` from a daemon `get_transactions` response.
/// A mempool transaction reports 0 and is treated as "no height yet".
fn parse_tx_block_height(bytes: &[u8]) -> Result<Option<u64>> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    Ok(value
        .get("txs")
        .and_then(|txs| txs.as_array())
        .and_then(|txs| txs.first())
        .and_then(|tx| tx.get("block_height"))
        .and_then(|height| height.as_u64())
        .filter(|height| *height > 0))
}

fn normalize_url(url: &str) -> Result<reqwest::Url> {
    let url = if url.contains("://") {
        url.to_owned()
    } else {
        format!("http://{url}")
    };
    let mut url = reqwest::Url::parse(&url).with_context(|| format!("invalid daemon URL {url}"))?;
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

/// Resolve the proxy to use for a daemon URL.
///
/// Local URLs are connected to directly. Everything else uses the configured
/// Outpost proxy, defaulting to `socks5h` when the config omits a scheme.
/// Passing a bare `host:port` to reqwest would silently select an HTTP proxy
/// while Tor's SOCKS listener rejects the request with HTTP 501.
/// A remote daemon is only reached through Tor: with no proxy the client would connect
/// directly and expose the user's address (protocol review #7; the Dojo, price and
/// Electrum clients got the same rule in N-06). Loopback daemons, such as the local
/// forwarder, need no proxy.
fn require_tor_for_remote_daemon(base_url: &reqwest::Url, proxy: Option<&str>) -> Result<()> {
    let is_local = base_url.host_str().is_some_and(|host| {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if is_local || proxy.is_some_and(|proxy| !proxy.trim().is_empty()) {
        return Ok(());
    }
    anyhow::bail!(
        "refusing to reach the remote Monero daemon {} without Tor: no proxy is configured",
        base_url.host_str().unwrap_or("(unknown host)")
    )
}

fn daemon_proxy(base_url: &reqwest::Url, proxy: &str) -> Result<Option<reqwest::Url>> {
    let is_local = base_url.host_str().is_some_and(|host| {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if is_local {
        return Ok(None);
    }

    let proxy = if proxy.contains("://") {
        proxy.to_owned()
    } else {
        format!("socks5h://{proxy}")
    };
    reqwest::Url::parse(&proxy)
        .map(Some)
        .with_context(|| format!("invalid daemon proxy {proxy}"))
}

fn point(public_key: &PublicKey) -> Result<Point> {
    let bytes: [u8; 32] = public_key
        .as_bytes()
        .try_into()
        .context("public spend key is not 32 bytes")?;
    CompressedPoint::from(bytes)
        .decompress()
        .ok_or_else(|| anyhow::anyhow!("public spend key is not a valid Ed25519 point"))
}

fn scalar(bytes: &[u8]) -> Result<Zeroizing<Scalar>> {
    let mut bytes = bytes;
    Ok(Zeroizing::new(
        Scalar::read(&mut bytes).context("key is not a canonical Ed25519 scalar")?,
    ))
}

/// The `get_transactions` request body for one txid.
///
/// The txid can come from the other party (Alice's transfer proof). It used to
/// be pasted into a JSON string unchecked, so a quote or bracket in it changed
/// the request sent to our own daemon (audit P-3). It must be exactly 64 hex
/// characters, and the body is built with a JSON serializer.
fn get_transactions_body(txid: &str) -> Result<String> {
    let txid = txid.trim();
    if txid.len() != 64 || !txid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("invalid Monero txid (expected 64 hex characters)");
    }
    Ok(serde_json::json!({
        "txs_hashes": [txid.to_ascii_lowercase()],
        "decode_as_json": false,
    })
    .to_string())
}

fn decode_txid(txid: &TxHash) -> Result<[u8; 32]> {
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(&txid.0, &mut bytes)
        .with_context(|| format!("invalid Monero txid {}", txid))?;
    Ok(bytes)
}

fn address(address: &Address, network: MoneroNetwork) -> Result<MoneroAddress> {
    let network = match network {
        MoneroNetwork::Mainnet => Network::Mainnet,
        MoneroNetwork::Stagenet => Network::Stagenet,
        MoneroNetwork::Testnet => Network::Testnet,
    };
    MoneroAddress::from_str(network, &address.to_string())
        .with_context(|| format!("invalid Monero address {address}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_lock_block_height_from_a_daemon_response() {
        let confirmed = br#"{"txs":[{"block_height":159486,"tx_hash":"ab"}],"status":"OK"}"#;
        assert_eq!(parse_tx_block_height(confirmed).unwrap(), Some(159486));

        // Mempool entries report 0 and must not be used as a scan start.
        let mempool = br#"{"txs":[{"block_height":0}]}"#;
        assert_eq!(parse_tx_block_height(mempool).unwrap(), None);

        let missing = br#"{"txs":[]}"#;
        assert_eq!(parse_tx_block_height(missing).unwrap(), None);

        assert!(parse_tx_block_height(b"not json").is_err());
    }

    #[test]
    fn a_transaction_status_is_mined_in_the_pool_or_unknown() {
        let mined = br#"{"txs":[{"block_height":159486,"in_pool":false}]}"#;
        assert_eq!(parse_tx_status(mined).unwrap(), TxStatus::Mined(159486));

        // daemons report a placeholder height for pool transactions: in_pool decides
        let pool_zero = br#"{"txs":[{"block_height":0,"in_pool":true}]}"#;
        let pool_max = br#"{"txs":[{"block_height":18446744073709551615,"in_pool":true}]}"#;
        assert_eq!(parse_tx_status(pool_zero).unwrap(), TxStatus::InPool);
        assert_eq!(parse_tx_status(pool_max).unwrap(), TxStatus::InPool);

        let missing = br#"{"status":"OK","missed_tx":["ab"]}"#;
        let empty = br#"{"txs":[]}"#;
        let unplaced = br#"{"txs":[{"block_height":0,"in_pool":false}]}"#;
        assert_eq!(parse_tx_status(missing).unwrap(), TxStatus::Unknown);
        assert_eq!(parse_tx_status(empty).unwrap(), TxStatus::Unknown);
        assert_eq!(parse_tx_status(unplaced).unwrap(), TxStatus::Unknown);
        assert!(parse_tx_status(b"not json").is_err());
    }

    #[test]
    fn normalize_url_adds_scheme_and_trailing_slash() {
        assert_eq!(
            normalize_url("node.example.org:18081").unwrap().as_str(),
            "http://node.example.org:18081/"
        );
        assert_eq!(
            normalize_url("https://node.example.org:18081")
                .unwrap()
                .as_str(),
            "https://node.example.org:18081/"
        );
    }

    #[test]
    fn local_daemon_urls_bypass_the_proxy() {
        for url in [
            "http://127.0.0.1:54247/",
            "http://[::1]:18081/",
            "http://localhost:18081/",
        ] {
            let url = normalize_url(url).unwrap();
            assert!(
                daemon_proxy(&url, "127.0.0.1:54236").unwrap().is_none(),
                "{url} must not be routed through the proxy"
            );
        }
    }

    #[test]
    fn remote_daemon_proxy_defaults_to_socks5h() {
        let url = normalize_url("http://node.example.org:18081").unwrap();
        assert_eq!(
            daemon_proxy(&url, "127.0.0.1:54236")
                .unwrap()
                .unwrap()
                .as_str(),
            "socks5h://127.0.0.1:54236"
        );
    }

    #[test]
    fn explicit_daemon_proxy_scheme_is_preserved() {
        let url = normalize_url("http://node.example.org:18081").unwrap();
        assert_eq!(
            daemon_proxy(&url, "socks5://127.0.0.1:9050")
                .unwrap()
                .unwrap()
                .as_str(),
            "socks5://127.0.0.1:9050"
        );
    }

    #[test]
    fn decode_txid_rejects_bad_length() {
        assert!(decode_txid(&TxHash("00".to_owned())).is_err());
    }

    #[test]
    fn scalar_rejects_non_canonical_bytes() {
        assert!(scalar(&[0xff; 32]).is_err());
    }

    struct AlwaysReceived;

    impl outpost_monero_wallet::HasReceivedOutputs for AlwaysReceived {
        fn has_received_outputs<'a>(
            &'a self,
            _: [u8; 32],
            _: [u8; 32],
            _: u64,
            _: Option<u64>,
            _: Option<backoff::ExponentialBackoff>,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<bool>> + Send + 'a>> {
            Box::pin(async { Ok(true) })
        }
    }

    #[tokio::test]
    async fn protocol_wallet_boundary_dispatches() {
        let wallets = outpost_monero_wallet::Wallets::new(Arc::new(AlwaysReceived));
        assert!(wallets
            .has_received_outputs([0; 32], [0; 32], 0, None, None)
            .await
            .unwrap());
    }
}

#[cfg(test)]
mod get_transactions_body_tests {
    use super::get_transactions_body;

    const TXID: &str = "380ca7e132b51b3888c1ed9035ad11bac7b2c8bef92e30d3e2c8dd9fc6b8a532";

    #[test]
    fn a_valid_txid_builds_the_expected_body() {
        let body: serde_json::Value = serde_json::from_str(&get_transactions_body(TXID).unwrap()).unwrap();
        assert_eq!(body["txs_hashes"][0], TXID);
        assert_eq!(body["decode_as_json"], false);
    }

    #[test]
    fn surrounding_whitespace_and_upper_case_are_normalised() {
        let body: serde_json::Value =
            serde_json::from_str(&get_transactions_body(&format!("  {}\n", TXID.to_uppercase())).unwrap()).unwrap();
        assert_eq!(body["txs_hashes"][0], TXID);
    }

    #[test]
    fn injected_json_is_refused() {
        // 64 characters, but with a quote and bracket in them
        let injected = format!("{}\"],\"decode_as_json\":true,\"x\":[\"", &TXID[..40]);
        assert!(get_transactions_body(&injected).is_err());
        assert!(get_transactions_body("abc\"]}").is_err());
    }

    #[test]
    fn a_wrong_length_or_non_hex_txid_is_refused() {
        assert!(get_transactions_body("").is_err());
        assert!(get_transactions_body(&TXID[..63]).is_err());
        assert!(get_transactions_body(&format!("{TXID}0")).is_err());
        assert!(get_transactions_body(&"g".repeat(64)).is_err());
    }
}

#[cfg(test)]
mod tor_requirement_tests {
    use super::*;

    fn url(text: &str) -> reqwest::Url {
        normalize_url(text).unwrap()
    }

    #[test]
    fn a_remote_daemon_without_a_proxy_is_refused() {
        for daemon in ["https://node.example.org:18081", "http://abc.onion:18089", "203.0.113.7:18081"] {
            assert!(require_tor_for_remote_daemon(&url(daemon), None).is_err(), "{daemon}");
            assert!(require_tor_for_remote_daemon(&url(daemon), Some("")).is_err(), "{daemon}");
            assert!(require_tor_for_remote_daemon(&url(daemon), Some("  ")).is_err(), "{daemon}");
        }
    }

    #[test]
    fn a_remote_daemon_with_a_proxy_is_allowed() {
        assert!(require_tor_for_remote_daemon(&url("https://node.example.org:18081"), Some("127.0.0.1:9050")).is_ok());
    }

    #[test]
    fn a_local_daemon_needs_no_proxy() {
        for daemon in ["http://127.0.0.1:52000", "localhost:18081", "http://[::1]:18081"] {
            assert!(require_tor_for_remote_daemon(&url(daemon), None).is_ok(), "{daemon}");
        }
    }
}

#[cfg(test)]
mod constructed_sweep_tests {
    use super::*;

    /// A real stagenet sweep (mined at block 2246652); see `swap-serde/tests/stored_sweep.rs`.
    const SWEEP_HEX: &str = include_str!("fixtures/stagenet_sweep_tx.hex");
    const SWEEP_TXID: &str = "ed134b4707d78f224145228178539c0c76463290defef9e7ff5af1189d935644";

    fn sweep_from_hex() -> ConstructedSweep {
        let bytes = hex::decode(SWEEP_HEX.trim()).unwrap();
        let transaction = Transaction::<NotPruned>::read(&mut bytes.as_slice()).unwrap();
        ConstructedSweep::from_transaction(transaction)
    }

    #[test]
    fn a_stored_sweep_comes_back_as_the_same_transaction() {
        let sweep = sweep_from_hex();
        assert_eq!(sweep.tx_hash.0, SWEEP_TXID);
        assert_eq!(sweep.tx_hex, SWEEP_HEX.trim());

        // what the resend path does with the stored row
        let again = ConstructedSweep::from_transaction(sweep.transaction().unwrap());
        assert_eq!(again.tx_hash.0, SWEEP_TXID);
        assert_eq!(again.tx_hex, sweep.tx_hex);
    }

    #[test]
    fn a_damaged_stored_sweep_is_an_error_not_a_panic() {
        let mut sweep = sweep_from_hex();
        sweep.tx_hex.truncate(sweep.tx_hex.len() - 20);
        assert!(sweep.transaction().is_err());
        sweep.tx_hex = "zz".to_owned();
        assert!(sweep.transaction().is_err());
    }
}
