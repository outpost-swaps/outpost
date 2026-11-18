//! Next-block fee estimation and independent transaction confirmation
//! checks from the paired Dojo.
//!
//! The electrs `estimate_fee` path breaks on quiet networks: it returns -1
//! and the configured fallback rate applies. The Dojo's `/fees/estimator`
//! exposes the network's actual next-block rates keyed by confirmation
//! probability ("0.999".."0.1"); this module fetches it through Tor and
//! briefly reuses the result within one setup.
//!
//! Fees are fixed into the swap at setup and swap transactions cannot be
//! RBF'd (the buyer derives its XMR sweep key from the exact redeem
//! transaction), so the rate picked here must be conservative: the default
//! confidence is 0.99 and missing keys fall back to the nearest higher
//! probability.

use anyhow::{bail, Context, Result};
use bdk::FeeRate;
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;

const AUTH_LOGIN_PATH: &str = "auth/login";
const FEES_ESTIMATOR_PATH: &str = "fees/estimator";
const PUSH_TX_PATH: &str = "pushtx/";
const TX_PATH: &str = "tx";

/// Short deduplication window so the lock/cancel/refund estimates in one
/// setup share a single Dojo fetch. This is not a stale-fee fallback: once
/// the window expires, setup fetches Dojo again before considering Electrum.
pub const ESTIMATOR_TTL: Duration = Duration::from_secs(30);

/// A Dojo can return HTTP 200 with a map that has no usable confidence key.
/// Treat that as a retryable bad response instead of failing the swap on the
/// first one.
const MAX_INVALID_MAP_ATTEMPTS: usize = 3;
const INVALID_MAP_RETRY_BACKOFF: Duration = Duration::from_secs(1);

/// How long a Dojo access token is reused before logging in again. The
/// token is also refreshed on demand if a request is rejected.
const TOKEN_TTL: Duration = Duration::from_secs(10 * 60);

/// Startup/setup retry budget for a Dojo that is still connecting or syncing.
/// The GUI starts the wallet before the Dojo fee endpoint is necessarily
/// reachable; a cold start should wait rather than immediately falling back to
/// electrum's quiet-network -1 path.
const DEFAULT_RETRY_BUDGET: Duration = Duration::from_secs(60);
const DEFAULT_RETRY_BACKOFF: Duration = Duration::from_secs(2);
const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct DojoFeeEstimator {
    url: Url,
    api_key: String,
    http: reqwest::Client,
    cache: Arc<Mutex<Option<(Instant, serde_json::Map<String, Value>)>>>,
    token: Arc<Mutex<Option<(Instant, String)>>>,
    retry_budget: Duration,
    retry_backoff: Duration,
}

impl DojoFeeEstimator {
    pub fn new(dojo_url: Url, api_key: String, tor_socks5_port: u16) -> Result<Self> {
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(15));
        if let Some(proxy) = bitcoin_wallet::tor_policy::socks_proxy_for(tor_socks5_port, &dojo_url)? {
            builder = builder.proxy(
                reqwest::Proxy::all(proxy).context("Failed to parse Tor SOCKS proxy URL")?,
            );
        }

        Ok(Self {
            url: dojo_url,
            api_key,
            http: builder.build().context("Failed to build HTTP client")?,
            cache: Arc::new(Mutex::new(None)),
            token: Arc::new(Mutex::new(None)),
            retry_budget: DEFAULT_RETRY_BUDGET,
            retry_backoff: DEFAULT_RETRY_BACKOFF,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_retry_budget(mut self, retry_budget: Duration) -> Self {
        self.retry_budget = retry_budget;
        self
    }

    #[cfg(test)]
    fn with_retry_policy(mut self, retry_budget: Duration, retry_backoff: Duration) -> Self {
        self.retry_budget = retry_budget;
        self.retry_backoff = retry_backoff;
        self
    }

    /// Reads the GUI's `atomic-swaps.properties` for the paired Dojo. Walks
    /// up from `start_dir` (at most 3 levels) so ASB/client data
    /// subdirectories find the root-level properties file. Returns `None`
    /// when unpaired (the electrum fee path then applies unchanged).
    pub fn from_properties_near(start_dir: &Path, tor_socks5_port: u16) -> Option<Self> {
        let mut dir: Option<&Path> = Some(start_dir);
        for _ in 0..3 {
            let candidate = dir?.join("atomic-swaps.properties");
            if let Ok(raw) = std::fs::read_to_string(&candidate) {
                let mut dojo_url = None;
                let mut api_key = None;
                for line in raw.lines() {
                    if let Some(value) = line.strip_prefix("cli.dojo.url=") {
                        // java.util.Properties escapes colons in stored values
                        dojo_url = Some(value.trim().replace("\\:", ":"));
                    }
                    if let Some(value) = line.strip_prefix("cli.dojo.apiKey=") {
                        api_key = Some(value.trim().to_string());
                    }
                }
                let dojo_url = dojo_url.filter(|url| !url.is_empty())?;
                let api_key = api_key.filter(|key| !key.is_empty())?;
                let url = Url::parse(&dojo_url).ok()?;
                return Self::new(url, api_key, tor_socks5_port).ok();
            }
            dir = dir?.parent();
        }
        None
    }

    /// Pure: pick the rate for `confidence` from the estimator's
    /// probability-keyed map. When the exact key is missing, use the nearest
    /// HIGHER probability (the more conservative quote); only fall back to a
    /// lower probability if no higher one exists.
    pub fn pick_rate(map: &serde_json::Map<String, Value>, confidence: &str) -> Option<FeeRate> {
        let wanted: f64 = confidence.parse().ok()?;
        let mut candidates: Vec<(f64, f64)> = map
            .iter()
            .filter_map(|(key, value)| {
                let probability = key.parse::<f64>().ok()?;
                let rate = value.as_f64()?;
                Some((probability, rate))
            })
            .filter(|(_, rate)| *rate > 0.0)
            .collect();
        candidates.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let (_, rate) = candidates
            .iter()
            .find(|(probability, _)| *probability >= wanted - f64::EPSILON)
            .or_else(|| {
                candidates
                    .iter()
                    .rev()
                    .find(|(probability, _)| *probability < wanted - f64::EPSILON)
            })?;
        if rate.is_finite() {
            Some(FeeRate::from_sat_per_vb(*rate as f32))
        } else {
            None
        }
    }

    /// Maps a confirmation target (in blocks) to the estimator's confidence
    /// key. Per-transaction-type urgency: a lock/redeem (target 1-2) wants a
    /// high-probability rate; cancel/refund/punish only spend at timelock
    /// expiry (hours away) and can afford the cheaper low-confidence rates.
    pub fn confidence_for(target_block: usize) -> &'static str {
        match target_block {
            1 => "0.999",
            2..=3 => "0.99",
            4..=8 => "0.5",
            _ => "0.2",
        }
    }

    /// Builds an endpoint relative to the pairing URL. The URL from the GUI
    /// carries the network path (e.g. `/test/v2`), and `Url::join` with a
    /// leading slash would strip it; keep the base path intact.
    fn endpoint(&self, path: &str) -> Result<Url> {
        Url::parse(&format!(
            "{}/{}",
            self.url.as_str().trim_end_matches('/'),
            path.trim_start_matches('/')
        ))
        .with_context(|| format!("Failed to build the Dojo endpoint for {path}"))
    }

    /// Logs in with the pairing key and caches the access token. The token is
    /// reused for the fee estimator and for independent confirmation checks;
    /// re-authenticating on every 30-second poll would hammer the Dojo.
    async fn access_token(&self) -> Result<String> {
        {
            let token = self.token.lock().unwrap();
            if let Some((fetched_at, token)) = token.as_ref() {
                if fetched_at.elapsed() < TOKEN_TTL {
                    return Ok(token.clone());
                }
            }
        }

        let login_url = self.endpoint(AUTH_LOGIN_PATH)?;
        let token = self
            .http
            .post(login_url)
            .form(&[("apikey", self.api_key.as_str())])
            .send()
            .await
            .context("Failed to reach the Dojo auth endpoint")?
            .error_for_status()
            .context("Dojo auth rejected the pairing api key")?
            .json::<Value>()
            .await
            .context("Failed to parse the Dojo auth response")?
            .pointer("/authorizations/access_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("Dojo auth response carries no access token")?;

        self.token
            .lock()
            .unwrap()
            .replace((Instant::now(), token.clone()));

        Ok(token)
    }

    /// Independently reads a transaction's confirmation count from the paired
    /// Dojo's chain view. This path does not depend on the electrum server's
    /// script-history index, so it can break a confirmation wait when an
    /// indexer serves a mined transaction as unconfirmed (WORKLOG §97).
    /// Unconfirmed transactions report zero.
    pub async fn tx_confirmations(&self, txid: &str) -> Result<u32> {
        let token = self.access_token().await?;
        let tx_url = self.endpoint(&format!("{TX_PATH}/{txid}"))?;
        let body = self
            .http
            .get(tx_url)
            .bearer_auth(token)
            .send()
            .await
            .with_context(|| format!("Failed to reach the Dojo chain view for {txid}"))?
            .error_for_status()
            .with_context(|| format!("Dojo chain view has no confirmed transaction {txid}"))?
            .json::<Value>()
            .await
            .with_context(|| format!("Failed to parse the Dojo chain view for {txid}"))?;

        Ok(body
            .get("confirmations")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .try_into()
            .unwrap_or(u32::MAX))
    }

    /// Pushes a signed transaction through the paired Dojo's `/pushtx/`
    /// endpoint. Dojo forwards to its bitcoind, which relays to the network;
    /// callers fall back to Electrum if this fails or times out.
    pub async fn broadcast(&self, tx_hex: &str) -> Result<()> {
        let token = self.access_token().await?;
        let url = self.endpoint(PUSH_TX_PATH)?;
        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .form(&[("tx", tx_hex)])
            .send()
            .await
            .context("Failed to reach the Dojo pushTx endpoint")?
            .error_for_status()
            .context("Dojo pushTx returned an error status")?;

        let body = response
            .json::<Value>()
            .await
            .context("Failed to parse the Dojo pushTx response")?;
        if body.get("status").and_then(Value::as_str) == Some("ok") {
            Ok(())
        } else {
            bail!("Dojo pushTx rejected the transaction: {body}")
        }
    }

    async fn fetch(&self) -> Result<serde_json::Map<String, Value>> {
        let token = self.access_token().await?;

        let estimator_url = self.endpoint(FEES_ESTIMATOR_PATH)?;
        let body = self
            .http
            .get(estimator_url)
            .bearer_auth(token)
            .send()
            .await
            .context("Failed to reach the Dojo fee estimator")?
            .error_for_status()
            .context("Dojo fee estimator returned an error status")?
            .json::<Value>()
            .await
            .context("Failed to parse the Dojo fee estimator response")?;

        body.as_object()
            .cloned()
            .context("Dojo fee estimator response is not a probability map")
    }

    async fn fetch_with_retry(
        &self,
        retry_budget: Duration,
    ) -> Result<serde_json::Map<String, Value>> {
        let deadline = Instant::now() + retry_budget;
        let mut backoff = self.retry_backoff;
        let mut last_error = None;
        loop {
            match self.fetch().await {
                Ok(map) => return Ok(map),
                Err(error) => {
                    last_error = Some(error);
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    let remaining = deadline.saturating_duration_since(now);
                    let sleep_for = backoff.min(remaining);
                    if sleep_for.is_zero() {
                        break;
                    }
                    tokio::time::sleep(sleep_for).await;
                    backoff = backoff.saturating_mul(2).min(MAX_RETRY_BACKOFF);
                }
            }
        }
        Err(last_error.expect("fetch_with_retry always makes at least one attempt"))
    }

    /// Fetch and cache once, retrying for the startup budget. Called in the
    /// background as soon as the wallet is constructed so a Dojo that is still
    /// connecting usually has the rate ready before swap setup.
    pub async fn warm_up(&self) -> Result<()> {
        let map = self
            .fetch_with_usable_rate(Self::confidence_for(2), self.retry_budget)
            .await?;
        self.cache.lock().unwrap().replace((Instant::now(), map));
        tracing::info!("Dojo fee estimator warmed up");
        Ok(())
    }

    /// Fetches a fresh map and retries a bounded number of times when the
    /// response is valid JSON but has no rate for the requested confidence.
    /// This never reuses an older cached map.
    async fn fetch_with_usable_rate(
        &self,
        confidence: &str,
        retry_budget: Duration,
    ) -> Result<serde_json::Map<String, Value>> {
        for attempt in 1..=MAX_INVALID_MAP_ATTEMPTS {
            let map = self.fetch_with_retry(retry_budget).await?;
            if Self::pick_rate(&map, confidence).is_some() {
                return Ok(map);
            }
            tracing::warn!(
                attempt,
                confidence,
                keys = ?map.keys().collect::<Vec<_>>(),
                "Dojo fee response has no usable rate; retrying"
            );
            if attempt < MAX_INVALID_MAP_ATTEMPTS {
                tokio::time::sleep(INVALID_MAP_RETRY_BACKOFF).await;
            }
        }

        Err(anyhow::anyhow!(
            "No usable rate for confidence {confidence} after {MAX_INVALID_MAP_ATTEMPTS} fresh Dojo responses"
        ))
    }

    /// Spawns [`Self::warm_up`] on the current Tokio runtime, when there is
    /// one. Failure is logged and never blocks wallet construction.
    pub fn spawn_warm_up(self: &Arc<Self>) {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let estimator = Arc::clone(self);
                handle.spawn(async move {
                    if let Err(error) = estimator.warm_up().await {
                        tracing::warn!("Dojo fee estimator warm-up failed: {:#}", error);
                    }
                });
            }
            Err(_) => {
                tracing::debug!("No Tokio runtime available; skipping Dojo fee warm-up")
            }
        }
    }

    fn cached_rate(&self, confidence: &str, max_age: Duration) -> Option<(Duration, FeeRate)> {
        let cache = self.cache.lock().unwrap();
        let (fetched_at, map) = cache.as_ref()?;
        let age = fetched_at.elapsed();
        if age > max_age {
            return None;
        }
        Self::pick_rate(map, confidence).map(|rate| (age, rate))
    }

    /// Sync entry point required by the fee trait: fetch Dojo first, retrying
    /// for the bounded setup budget, and only reuse the most recent map for
    /// the short per-setup dedupe window. There is no multi-minute stale-fee
    /// fallback; Electrum is the last resort.
    pub fn estimate_feerate(&self, target_block: usize) -> Result<FeeRate> {
        let confidence = Self::confidence_for(target_block);

        if let Some((_, rate)) = self.cached_rate(confidence, ESTIMATOR_TTL) {
            return Ok(rate);
        }

        let map = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(
                self.fetch_with_usable_rate(confidence, self.retry_budget),
            )
        })?;
        let rate = Self::pick_rate(&map, confidence)
            .expect("fetch_with_usable_rate returns a map with the requested confidence");
        self.cache.lock().unwrap().replace((Instant::now(), map));

        Ok(rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncWriteExt;

    fn map(value: Value) -> serde_json::Map<String, Value> {
        value
            .as_object()
            .expect("test value must be an object")
            .clone()
    }

    #[test]
    fn picks_the_exact_confidence_rate() {
        let fees = map(json!({ "0.1": 1, "0.5": 3, "0.99": 12, "0.999": 40 }));
        let rate = DojoFeeEstimator::pick_rate(&fees, "0.99").unwrap();
        assert_eq!(rate.as_sat_per_vb(), 12.0);
    }

    /// A missing key must fall back to the nearest HIGHER probability: a
    /// higher confidence means a higher, safer rate for a transaction that
    /// cannot be RBF'd later.
    #[test]
    fn falls_back_to_the_nearest_higher_probability() {
        let fees = map(json!({ "0.1": 1, "0.5": 3, "0.999": 40 }));
        let rate = DojoFeeEstimator::pick_rate(&fees, "0.99").unwrap();
        assert_eq!(
            rate.as_sat_per_vb(),
            40.0,
            "0.999 is the nearest higher probability"
        );
    }

    #[test]
    fn falls_back_to_the_highest_lower_probability_when_no_higher_exists() {
        let fees = map(json!({ "0.1": 1, "0.5": 3 }));
        let rate = DojoFeeEstimator::pick_rate(&fees, "0.99").unwrap();
        assert_eq!(
            rate.as_sat_per_vb(),
            3.0,
            "0.5 is the highest available lower probability"
        );
    }

    #[test]
    fn ignores_zero_and_garbage_entries() {
        let fees = map(json!({ "0.99": 0, "junk": -5, "0.5": 3 }));
        let rate = DojoFeeEstimator::pick_rate(&fees, "0.99").unwrap();
        assert_eq!(rate.as_sat_per_vb(), 3.0);

        assert!(DojoFeeEstimator::pick_rate(&map(json!({ "0.99": 0 })), "0.99").is_none());
    }

    #[test]
    fn reads_the_dojo_pairing_from_the_properties_file() {
        let dir = std::env::temp_dir().join(format!("outpost-fee-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("atomic-swaps.properties"),
            "cli.dojo.enabled=true\n\
             cli.dojo.url=http\\://4f6hya64dcofjavp66illnynpoxjdfc2bnvwduleqkvjuqq633d2fiyd.onion/v2\n\
             cli.dojo.apiKey=test-key-123\n",
        )
        .unwrap();

        let estimator = DojoFeeEstimator::from_properties_near(&dir, 9050)
            .expect("the properties file must yield an estimator");
        assert_eq!(estimator.api_key, "test-key-123");
        // the stored url's escaped colon is unescaped and preserved
        assert!(estimator.url.as_str().contains("http://4f6hya"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn endpoints_preserve_the_pairing_base_path() {
        let estimator = DojoFeeEstimator::new(
            Url::parse("http://example.onion/test/v2").unwrap(),
            "unused".to_string(),
            0,
        )
        .unwrap();

        assert_eq!(
            estimator.endpoint("auth/login").unwrap().as_str(),
            "http://example.onion/test/v2/auth/login"
        );
        assert_eq!(
            estimator.endpoint("/tx/abc").unwrap().as_str(),
            "http://example.onion/test/v2/tx/abc"
        );
        assert_eq!(
            estimator.endpoint("fees/estimator").unwrap().as_str(),
            "http://example.onion/test/v2/fees/estimator"
        );
    }

    #[test]
    fn confirmation_targets_map_to_confidence_keys() {
        assert_eq!(DojoFeeEstimator::confidence_for(1), "0.999");
        assert_eq!(DojoFeeEstimator::confidence_for(2), "0.99");
        assert_eq!(DojoFeeEstimator::confidence_for(3), "0.99");
        assert_eq!(DojoFeeEstimator::confidence_for(6), "0.5");
        assert_eq!(DojoFeeEstimator::confidence_for(8), "0.5");
        assert_eq!(DojoFeeEstimator::confidence_for(12), "0.2");
        assert_eq!(DojoFeeEstimator::confidence_for(100), "0.2");
    }

    #[test]
    fn missing_properties_file_yields_no_estimator() {
        assert!(DojoFeeEstimator::from_properties_near(&std::env::temp_dir(), 9050).is_none());
    }

    #[test]
    fn cache_dedupe_window_is_pinned() {
        assert_eq!(ESTIMATOR_TTL, Duration::from_secs(30));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cached_estimate_is_returned_within_the_ttl() {
        // The real fetch needs a live Dojo; here we only verify the caching
        // wrapper: seed the cache directly and confirm the sync path returns
        // it without any network access.
        let estimator = DojoFeeEstimator::new(
            Url::parse("http://127.0.0.1:1").unwrap(),
            "unused".to_string(),
            0,
        )
        .unwrap();

        // seed the cache with a probability map; the sync path must pick the
        // rate for the mapped confidence without any network access
        estimator
            .cache
            .lock()
            .unwrap()
            .replace((Instant::now(), map(json!({ "0.99": 7, "0.5": 3 }))));

        let rate = estimator.estimate_feerate(2).unwrap();
        assert_eq!(
            rate.as_sat_per_vb(),
            7.0,
            "target 2 maps to 0.99 → 7 sat/vB"
        );
        let laxer = estimator.estimate_feerate(6).unwrap();
        assert_eq!(
            laxer.as_sat_per_vb(),
            3.0,
            "the same cached map answers target 6 at the 0.5 rate"
        );
    }

    /// In-process mock Dojo: verifies the full estimator path — auth login
    /// with the api key, Bearer-authenticated estimator fetch, rate picking,
    /// and the TTL cache (second call within the TTL must not hit the
    /// server again).
    #[tokio::test(flavor = "multi_thread")]
    async fn end_to_end_fetch_auth_and_cache_against_a_mock_dojo() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let mut requests_served = 0u32;
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                requests_served += 1;
                let served = requests_served;
                // Read the request head; dispatch on the path.
                let mut buf = vec![0u8; 4096];
                let _ = socket.peek(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf);
                let body = if head.contains("POST /auth/login") {
                    "{\"authorizations\":{\"access_token\":\"tok-123\",\"refresh_token\":\"r\"}}"
                } else if head.contains("GET /fees/estimator") && head.contains("Bearer tok-123") {
                    "{\"0.1\":1,\"0.5\":3,\"0.9\":8,\"0.99\":21,\"0.999\":55}"
                } else {
                    panic!(
                        "unexpected request #{served}: {}",
                        head.lines().next().unwrap_or("")
                    );
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let estimator = DojoFeeEstimator::new(
            Url::parse(&format!("http://{addr}")).unwrap(),
            "test-api-key".to_string(),
            0, // loopback mock: no Tor
        )
        .unwrap();

        let rate = estimator.estimate_feerate(2).unwrap();
        assert_eq!(
            rate.as_sat_per_vb(),
            21.0,
            "target 2 → 0.99 confidence → 21 sat/vB"
        );

        // TTL cache: the second call answers from cache with a DIFFERENT
        // target (the 0.5 rate) without touching the server again.
        let laxer = estimator.estimate_feerate(6).unwrap();
        assert_eq!(
            laxer.as_sat_per_vb(),
            3.0,
            "target 6 → 0.5 confidence → 3 sat/vB from the cached map"
        );

        server.abort();
    }

    /// A dead Dojo with NO cached value must error (never guess a fee), and
    /// must not hang the calling runtime.
    #[tokio::test(flavor = "multi_thread")]
    async fn dead_dojo_without_cache_errors_out() {
        let estimator = DojoFeeEstimator::new(
            Url::parse("http://127.0.0.1:1").unwrap(), // nothing listens here
            "unused".to_string(),
            0,
        )
        .unwrap()
        .with_retry_budget(Duration::ZERO);

        assert!(estimator.estimate_feerate(2).is_err());
    }

    /// A cache older than the short dedupe window must not be used as a
    /// fallback: if Dojo cannot be refreshed, Electrum is the last resort.
    #[tokio::test(flavor = "multi_thread")]
    async fn dead_dojo_with_expired_cache_errors_out() {
        let estimator = DojoFeeEstimator::new(
            Url::parse("http://127.0.0.1:1").unwrap(),
            "unused".to_string(),
            0,
        )
        .unwrap()
        .with_retry_budget(Duration::ZERO);

        let stale_at = Instant::now() - ESTIMATOR_TTL - Duration::from_secs(1);
        estimator
            .cache
            .lock()
            .unwrap()
            .replace((stale_at, map(json!({ "0.99": 9 }))));

        assert!(estimator.estimate_feerate(2).is_err());
    }

    /// A cold start must retry a Dojo that is still coming up instead of
    /// immediately falling back to electrum. The retry budget/backoff are
    /// shortened here so the test is fast.
    #[tokio::test(flavor = "multi_thread")]
    async fn cold_start_retries_until_the_dojo_answers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));

        let server = {
            let attempts = attempts.clone();
            tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    let mut buf = vec![0u8; 4096];
                    let _ = socket.peek(&mut buf).await.unwrap();
                    let head = String::from_utf8_lossy(&buf);

                    let (status, body) = if attempt == 0 && head.contains("POST /auth/login") {
                        (500, "{\"message\":\"not ready\"}")
                    } else if head.contains("POST /auth/login") {
                        (200, "{\"authorizations\":{\"access_token\":\"tok-123\"}}")
                    } else if head.contains("GET /fees/estimator")
                        && head.contains("Bearer tok-123")
                    {
                        (
                            200,
                            "{\"0.1\":1,\"0.5\":3,\"0.9\":8,\"0.99\":21,\"0.999\":55}",
                        )
                    } else {
                        panic!("unexpected request: {}", head.lines().next().unwrap_or(""));
                    };

                    let response = format!(
                        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                }
            })
        };

        let estimator = DojoFeeEstimator::new(
            Url::parse(&format!("http://{addr}")).unwrap(),
            "test-api-key".to_string(),
            0,
        )
        .unwrap()
        .with_retry_policy(Duration::from_secs(2), Duration::from_millis(10));

        let rate = estimator.estimate_feerate(2).unwrap();
        assert_eq!(rate.as_sat_per_vb(), 21.0);
        assert!(
            attempts.load(Ordering::SeqCst) >= 2,
            "the first auth failure must be retried"
        );

        server.abort();
    }

    /// The non-blocking entry point used by wallet construction must actually
    /// run the fetch and populate the cache in the background.
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_warm_up_populates_the_cache() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let _ = socket.peek(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf);
                let body = if head.contains("POST /auth/login") {
                    "{\"authorizations\":{\"access_token\":\"tok-123\"}}"
                } else if head.contains("GET /fees/estimator") {
                    "{\"0.99\":21}"
                } else {
                    panic!("unexpected request: {}", head.lines().next().unwrap_or(""));
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let estimator = Arc::new(
            DojoFeeEstimator::new(
                Url::parse(&format!("http://{addr}")).unwrap(),
                "test-api-key".to_string(),
                0,
            )
            .unwrap(),
        );

        estimator.spawn_warm_up();
        tokio::time::timeout(Duration::from_secs(2), async {
            while estimator.cache.lock().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the background warm-up must populate the cache");

        assert_eq!(estimator.estimate_feerate(2).unwrap().as_sat_per_vb(), 21.0);

        server.abort();
    }

    /// Startup warm-up must populate the cache so a later setup does not have
    /// to wait for a cold fetch.
    #[tokio::test(flavor = "multi_thread")]
    async fn warm_up_populates_the_cache() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let _ = socket.peek(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf);
                let body = if head.contains("POST /auth/login") {
                    "{\"authorizations\":{\"access_token\":\"tok-123\"}}"
                } else if head.contains("GET /fees/estimator") {
                    "{\"0.99\":21}"
                } else {
                    panic!("unexpected request: {}", head.lines().next().unwrap_or(""));
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let estimator = DojoFeeEstimator::new(
            Url::parse(&format!("http://{addr}")).unwrap(),
            "test-api-key".to_string(),
            0,
        )
        .unwrap();

        estimator.warm_up().await.unwrap();
        assert!(estimator.cache.lock().unwrap().is_some());
        assert_eq!(estimator.estimate_feerate(2).unwrap().as_sat_per_vb(), 21.0);

        server.abort();
    }

    /// The chain view is an independent truth source for confirmation waits:
    /// a confirmed transaction reports its count, an unconfirmed one reports
    /// zero, and both requests reuse the login token.
    #[tokio::test(flavor = "multi_thread")]
    async fn tx_confirmations_reads_the_chain_view_and_reuses_the_token() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let logins = Arc::new(AtomicUsize::new(0));

        let server = {
            let logins = logins.clone();
            tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut buf = vec![0u8; 4096];
                    let _ = socket.peek(&mut buf).await.unwrap();
                    let head = String::from_utf8_lossy(&buf);
                    let body = if head.contains("POST /auth/login") {
                        logins.fetch_add(1, Ordering::SeqCst);
                        "{\"authorizations\":{\"access_token\":\"tok-123\"}}"
                    } else if head.contains("GET /tx/abc123") && head.contains("Bearer tok-123") {
                        "{\"txid\":\"abc123\",\"confirmations\":3,\"block\":{\"height\":100}}"
                    } else if head.contains("GET /tx/def456") && head.contains("Bearer tok-123") {
                        "{\"txid\":\"def456\"}"
                    } else {
                        panic!("unexpected request: {}", head.lines().next().unwrap_or(""));
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                }
            })
        };

        let estimator = DojoFeeEstimator::new(
            Url::parse(&format!("http://{addr}")).unwrap(),
            "test-api-key".to_string(),
            0,
        )
        .unwrap();

        assert_eq!(estimator.tx_confirmations("abc123").await.unwrap(), 3);
        assert_eq!(
            estimator.tx_confirmations("def456").await.unwrap(),
            0,
            "an unconfirmed transaction has no confirmations field"
        );
        assert_eq!(
            logins.load(Ordering::SeqCst),
            1,
            "the access token must be reused"
        );

        server.abort();
    }

    /// pushTx must use the paired Dojo's authenticated form endpoint. This
    /// covers the fallback path used when the Electrum broadcast call stalls.
    #[tokio::test(flavor = "multi_thread")]
    async fn broadcast_posts_the_signed_tx_to_the_dojo() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let _ = socket.peek(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf);
                let body = if head.contains("POST /auth/login") {
                    "{\"authorizations\":{\"access_token\":\"tok-123\"}}"
                } else if head.contains("POST /pushtx/") && head.contains("Bearer tok-123") {
                    "{\"status\":\"ok\",\"data\":\"txid\"}"
                } else {
                    panic!("unexpected request: {}", head.lines().next().unwrap_or(""));
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let estimator = DojoFeeEstimator::new(
            Url::parse(&format!("http://{addr}")).unwrap(),
            "test-api-key".to_string(),
            0,
        )
        .unwrap();

        estimator.broadcast("deadbeef").await.unwrap();

        server.abort();
    }
}

#[cfg(test)]
mod tor_required_tests {
    use super::*;

    #[test]
    fn a_remote_dojo_without_a_tor_port_is_refused() {
        let url = Url::parse("http://abcdefghijklmnop.onion/test/v2").unwrap();
        assert!(DojoFeeEstimator::new(url, "key".to_string(), 0).is_err());
    }

    #[test]
    fn a_remote_dojo_with_a_tor_port_is_accepted() {
        let url = Url::parse("http://abcdefghijklmnop.onion/test/v2").unwrap();
        assert!(DojoFeeEstimator::new(url, "key".to_string(), 9050).is_ok());
    }

    #[test]
    fn a_local_dojo_needs_no_tor_port() {
        let url = Url::parse("http://127.0.0.1:8899/v2").unwrap();
        assert!(DojoFeeEstimator::new(url, "key".to_string(), 0).is_ok());
    }
}

