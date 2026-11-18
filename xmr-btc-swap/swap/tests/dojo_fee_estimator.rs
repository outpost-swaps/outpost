//! Integration coverage for the public Dojo fee-estimator path.
//!
//! These tests exercise the real HTTP/auth/cache code against an in-process
//! mock Dojo, including the pairing-file reader and the setup-time retry that
//! covers a Dojo which is still starting. Cache-expiry/floor math lives in
//! unit tests because it needs direct access to the private cache.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bitcoin_wallet::DojoFeeEstimator;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use url::Url;

struct MockDojo {
    addr: SocketAddr,
    logins: Arc<AtomicUsize>,
    estimator_requests: Arc<AtomicUsize>,
    tx_requests: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}

impl MockDojo {
    fn url(&self) -> String {
        format!("http://{}/test/v2", self.addr)
    }

    fn stop(self) {
        self.server.abort();
    }
}

/// The mock deliberately checks the full pairing base path (`/test/v2`) and
/// requires the Bearer token on the fee/chain endpoints. The optional first
/// login failure simulates a Dojo that is still coming up.
async fn spawn_mock_dojo(fail_first_login: bool) -> MockDojo {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let logins = Arc::new(AtomicUsize::new(0));
    let estimator_requests = Arc::new(AtomicUsize::new(0));
    let tx_requests = Arc::new(AtomicUsize::new(0));

    let server = {
        let logins = Arc::clone(&logins);
        let estimator_requests = Arc::clone(&estimator_requests);
        let tx_requests = Arc::clone(&tx_requests);
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let _ = socket.peek(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf);

                let (status, reason, body) = if head.contains("POST /test/v2/auth/login") {
                    let attempt = logins.fetch_add(1, Ordering::SeqCst);
                    if fail_first_login && attempt == 0 {
                        (
                            503,
                            "Service Unavailable",
                            "{\"message\":\"not ready\"}".to_string(),
                        )
                    } else {
                        (
                            200,
                            "OK",
                            "{\"authorizations\":{\"access_token\":\"tok-123\"}}".to_string(),
                        )
                    }
                } else if head.contains("GET /test/v2/fees/estimator")
                    && head.contains("Bearer tok-123")
                {
                    estimator_requests.fetch_add(1, Ordering::SeqCst);
                    (
                        200,
                        "OK",
                        "{\"0.1\":1,\"0.2\":1,\"0.5\":3,\"0.9\":8,\"0.99\":21,\"0.999\":55}".to_string(),
                    )
                } else if head.contains("GET /test/v2/tx/abc123") && head.contains("Bearer tok-123")
                {
                    tx_requests.fetch_add(1, Ordering::SeqCst);
                    (
                        200,
                        "OK",
                        "{\"txid\":\"abc123\",\"confirmations\":3,\"block\":{\"height\":100}}"
                            .to_string(),
                    )
                } else {
                    (
                        404,
                        "Not Found",
                        format!(
                            "{{\"message\":\"unexpected request: {}\"}}",
                            head.lines().next().unwrap_or("")
                        ),
                    )
                };

                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        })
    };

    MockDojo {
        addr,
        logins,
        estimator_requests,
        tx_requests,
        server,
    }
}

/// Pairing-file -> warm-up -> cached fee -> independent chain view, all over
/// real HTTP. The chain-view call must reuse the login token rather than
/// authenticating again.
#[tokio::test(flavor = "multi_thread")]
async fn warm_up_reads_pairing_file_and_reuses_the_token() {
    let mock = spawn_mock_dojo(false).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("atomic-swaps.properties"),
        format!(
            "cli.dojo.enabled=true\n\
             cli.dojo.url=http\\://127.0.0.1:{}/test/v2\n\
             cli.dojo.apiKey=integration-key-123\n",
            mock.addr.port()
        ),
    )
    .unwrap();

    let estimator = DojoFeeEstimator::from_properties_near(dir.path(), 0)
        .expect("the pairing file must yield an estimator");

    estimator.warm_up().await.unwrap();
    assert_eq!(mock.logins.load(Ordering::SeqCst), 1);
    assert_eq!(mock.estimator_requests.load(Ordering::SeqCst), 1);

    let rate = estimator.estimate_feerate(2).unwrap();
    assert_eq!(rate.to_sat_per_vb_ceil(), 21);
    assert_eq!(estimator.estimate_feerate(1).unwrap().to_sat_per_vb_ceil(), 55);
    assert_eq!(estimator.estimate_feerate(6).unwrap().to_sat_per_vb_ceil(), 3);
    assert_eq!(estimator.estimate_feerate(9).unwrap().to_sat_per_vb_ceil(), 1);
    assert_eq!(
        mock.estimator_requests.load(Ordering::SeqCst),
        1,
        "a fresh warm-up cache must answer setup without another fetch"
    );

    assert_eq!(estimator.tx_confirmations("abc123").await.unwrap(), 3);
    assert_eq!(mock.tx_requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        mock.logins.load(Ordering::SeqCst),
        1,
        "the chain view must reuse the cached access token"
    );

    mock.stop();
}

/// A cold setup must wait for a Dojo that is still starting instead of
/// immediately falling back to electrum. The first login returns 503; the
/// estimator retries on its normal backoff and then succeeds.
#[tokio::test(flavor = "multi_thread")]
async fn cold_start_retries_a_dojo_that_is_not_ready_yet() {
    let mock = spawn_mock_dojo(true).await;
    let estimator = DojoFeeEstimator::new(
        Url::parse(&mock.url()).unwrap(),
        "integration-key-123".to_string(),
        0,
    )
    .unwrap();

    let started = std::time::Instant::now();
    let rate = estimator.estimate_feerate(2).unwrap();

    assert_eq!(rate.to_sat_per_vb_ceil(), 21);
    assert_eq!(
        mock.logins.load(Ordering::SeqCst),
        2,
        "the failed first auth must be retried exactly once"
    );
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "the retry must wait for the backoff instead of hammering the Dojo"
    );

    mock.stop();
}
