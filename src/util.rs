use anyhow::{anyhow, Context, Result};
use jni::objects::JClass;
use jni::sys::{jboolean, jstring};
use jni::JNIEnv;
use std::convert::TryInto;
use std::ops::Not;
use std::time::{Duration, SystemTime};
use swap::network::download_rpc_result::DownloadRpcResult;
use swap::network::quote::SwapDisconnected;
use swap::seed::Seed;
use swap::swap_error::{ErrorType, SwapError};
use swap::{fs, util};
use uuid::Uuid;

#[derive(Debug)]
pub struct TryFromSliceError(());

pub(crate) fn decode_seed(seed_base64: &str) -> Result<Seed> {
    let bytes = base64::decode(seed_base64).context("Failed to decode base64 seed")?;
    let bytes: [u8; 64] = bytes
        .try_into()
        .map_err(|_| anyhow!("Seed must be 64 bytes"))?;
    Ok(Seed::from(bytes))
}

/// Gives the daemon the nodes that must agree about Alice's Monero lock. Nothing happens for an
/// empty list (the user's own node is trusted alone, or the network is a test network).
///
/// With `count` above zero, `urls` are candidates: each is checked first (reachable through Tor,
/// certificate accepted, answering) and the first `count` that work are used. If fewer work and
/// `refuse_if_short` is set (a new swap, before any Bitcoin moves) this is an error. A resumed
/// swap passes `false`: the candidates that did not work still fill the places, so the lock check
/// waits for them instead of passing without them.
pub(crate) async fn attach_witnesses(
    daemon: &swap::monero::ng::Daemon,
    urls: &[String],
    count: usize,
    required: bool,
    proxy: &str,
    refuse_if_short: bool,
) -> anyhow::Result<()> {
    // Recorded even with no URLs: required but empty means the lock check must fail, not pass.
    daemon.set_witnesses_required(required);
    if urls.is_empty() {
        return Ok(());
    }
    if count == 0 {
        tracing::info!(witnesses = ?urls, "Monero witness nodes will have to agree that Alice's lock exists");
        daemon.set_witnesses(swap::monero::witness::Witnesses::from_urls(urls, proxy));
        return Ok(());
    }

    let candidates = urls
        .iter()
        .map(|url| {
            std::sync::Arc::new(swap::monero::witness::RemoteWitness::new(url.clone(), proxy.to_owned()))
                as std::sync::Arc<dyn swap::monero::witness::LockWitness>
        })
        .collect();
    let chosen = swap::monero::witness::Witnesses::choose(
        candidates,
        count,
        swap::monero::witness::WITNESS_PREFLIGHT_TIMEOUT,
    )
    .await;
    for (name, reason) in &chosen.failed {
        tracing::warn!(witness = %name, %reason, "Monero witness candidate did not pass the pre-flight check");
    }
    if !chosen.is_complete() && refuse_if_short {
        anyhow::bail!(
            "Only {} of the {} Monero nodes needed to double-check the seller's lock answered ({}). \
             Nothing has been locked: try again in a few minutes, or mark your own Monero node as yours in Settings.",
            chosen.working,
            chosen.wanted,
            chosen
                .failed
                .iter()
                .map(|(name, reason)| format!("{name}: {reason}"))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    tracing::info!(
        witnesses = ?chosen.witnesses.names(),
        working = chosen.working,
        "Monero witness nodes will have to agree that Alice's lock exists"
    );
    daemon.set_witnesses(chosen.witnesses);
    Ok(())
}

/// Connect to the Monero daemon, retrying transient Tor/forwarder failures.
///
/// The first seconds after a swap client starts are exactly when the daemon
/// pool forwarder may not be listening yet; a single failed connect makes the
/// caller fall back to a stale wallet height and turn the redeem into a full
/// rescan.
pub(crate) async fn connect_monero_daemon(
    url: &str,
    proxy: Option<&str>,
) -> Option<swap::monero::ng::Daemon> {
    for attempt in 1..=3u32 {
        match swap::monero::ng::Daemon::connect(url, proxy, None).await {
            Ok(daemon) => {
                // Keep the output distribution on disk: downloading it in full
                // over an onion daemon took ~2 minutes per redeem.
                match fs::system_data_dir() {
                    Ok(dir) => daemon
                        .enable_distribution_cache(dir.join("monero").join("output-distribution.cache")),
                    Err(error) => tracing::warn!(%error, "No data directory for the output distribution cache"),
                }
                return Some(daemon);
            }
            Err(error) => {
                tracing::warn!(
                    attempt,
                    "Failed to connect to the Monero daemon: {error:#}"
                );
                if attempt < 3 {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    }
    None
}

/// Restore height for the redeem wallet: the block that contains the XMR lock.
///
/// The recorded restore height can be thousands of blocks behind (the check
/// wallet's cached height), which turns the redeem into a full rescan. The lock
/// transaction's own block bounds the scan without risking the locked output.
pub(crate) async fn xmr_lock_restore_height(
    monero_daemon: Option<&swap::monero::ng::Daemon>,
    xmr_lock_txid: &str,
) -> Option<u32> {
    let daemon = monero_daemon?;
    match daemon.tx_block_height(xmr_lock_txid).await {
        Ok(height) => height.map(|height| height as u32),
        Err(error) => {
            tracing::warn!(
                %error,
                "Could not read the XMR lock height; using the recorded restore height"
            );
            None
        }
    }
}

/// How many times building the Monero sweep is attempted. Alice has already
/// taken the Bitcoin by then, so a transient daemon or Tor failure (swap
/// `60889768`: "Decoy selection error ... error decoding response body") must not
/// end the swap while the Monero is still waiting to be swept.
const SWEEP_PREPARE_ATTEMPTS: u32 = 8;

/// Errors that retrying cannot fix: the keys or the destination address are
/// invalid. Everything else (daemon, decoys, Tor) is treated as transient.
pub(crate) fn sweep_prepare_error_is_permanent(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("invalid recovered xmr")
        || message.contains("invalid xmr redeem address")
}

/// Seconds to wait before attempt `next_attempt` (2..): 5, 10, 20, 40, 60, 60...
pub(crate) fn sweep_prepare_retry_delay_secs(next_attempt: u32) -> u64 {
    (5u64 << next_attempt.saturating_sub(2).min(4)).min(60)
}

async fn prepare_xmr_redeem_with_retry<F, Fut, T>(
    env: &JNIEnv<'_>,
    swap_id: &str,
    mut prepare: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut attempt = 1u32;
    loop {
        match prepare().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let message = format!("{error:#}");
                if attempt >= SWEEP_PREPARE_ATTEMPTS || sweep_prepare_error_is_permanent(&message) {
                    return Err(error);
                }
                attempt += 1;
                let delay = sweep_prepare_retry_delay_secs(attempt);
                tracing::warn!(attempt, delay, "Monero sweep preparation failed; retrying: {message}");
                swap::util::print_swap_log_ln(
                    env,
                    format!(
                        "Monero sweep preparation hit a network error ({}); retrying in {delay}s (attempt {attempt}/{SWEEP_PREPARE_ATTEMPTS}). Your Monero is safe.",
                        humanize_error(&message)
                    ),
                );
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            }
        }
    }
}

/// The amount the sweep pays, or `None` (logged) when it cannot be worked out: the GUI then
/// shows no number rather than a guess (matrix N-15).
fn received_or_log(
    state5: &swap::protocol_v5::machine::bob::State5,
    sweep: &swap::monero::ng::ConstructedSweep,
) -> Option<swap::protocol_v5::bob::XmrReceived> {
    match swap::protocol_v5::bob::xmr_received(state5, sweep) {
        Ok(received) => Some(received),
        Err(error) => {
            tracing::warn!("Could not work out the XMR received from the sweep: {error:#}");
            None
        }
    }
}

/// Tell the GUI the sweep is out, then (when known) exactly how much it pays.
fn report_sweep_broadcast(
    env: &JNIEnv<'_>,
    swap_id: &str,
    txid: &str,
    received: Option<swap::protocol_v5::bob::XmrReceived>,
) {
    swap::util::on_xmr_sweep_broadcast(env, swap_id.to_string(), txid.to_string());
    if let Some(received) = received {
        swap::util::on_xmr_received(
            env,
            swap_id.to_string(),
            received.received_piconero,
            received.fee_piconero,
        );
    }
}

/// Redeem the XMR for a protocol-5 swap.
///
/// Prefers the daemon-native sweep: monero-oxide builds and signs the sweep
/// directly from the lock transaction (`construct_sweep_to_single`), so there
/// is no redeem wallet to create, open, refresh or rescan. Wallet-RPC is only
/// used when no daemon handle exists.
pub(crate) async fn redeem_xmr(
    env: &JNIEnv<'_>,
    swap_id: &str,
    database: &swap::database::SqliteDatabase,
    swap_uuid: Uuid,
    state5: &swap::protocol_v5::machine::bob::State5,
    monero_wallet: &swap::monero::Wallet,
    monero_daemon: Option<&swap::monero::ng::Daemon>,
    monero_receive_address: monero::Address,
    env_config: swap::env::Config,
) -> Result<()> {
    if let Some(daemon) = monero_daemon {
        swap::util::call_basic_listener_method(
            env,
            swap_id.to_string(),
            swap::util::ON_START_REDEEM_XMR_SYNC_METHOD,
        );
        // A sweep signed by an earlier attempt (crash or stop after publishing) is sent or
        // awaited again instead of building a second one (matrix S-03). Its fee is in the
        // stored transaction, so a resumed swap shows the same amount (matrix N-15).
        let stored_received = swap::protocol_v5::bob::stored_sweep(database, swap_uuid)
            .await
            .ok()
            .flatten()
            .and_then(|sweep| received_or_log(state5, &sweep));
        if let Some(txid) = swap::protocol_v5::bob::resend_stored_xmr_redeem(
            database,
            swap_uuid,
            state5,
            daemon,
            |txid| report_sweep_broadcast(env, swap_id, txid, stored_received),
        )
        .await?
        {
            swap::protocol::Database::insert_redeem_sweep_txid(database, swap_uuid, txid).await?;
            return Ok(());
        }
        let destination = monero_receive_address.to_string();
        let sweep = prepare_xmr_redeem_with_retry(env, swap_id, || {
            swap::protocol_v5::bob::prepare_xmr_redeem(
                daemon,
                state5,
                &destination,
                env_config.monero_network,
            )
        })
        .await?;
        swap::util::call_basic_listener_method(
            env,
            swap_id.to_string(),
            swap::util::ON_START_XMR_SWEEP_METHOD,
        );
        let received = received_or_log(state5, &sweep);
        let txid = swap::protocol_v5::bob::publish_xmr_redeem(
            database,
            swap_uuid,
            state5,
            daemon,
            &sweep,
            |txid| report_sweep_broadcast(env, swap_id, txid, received),
        )
        .await?;
        // The wallet path recorded the sweep txid; the daemon path must too, or
        // history cannot link the swap to the Monero transaction.
        swap::protocol::Database::insert_redeem_sweep_txid(database, swap_uuid, txid).await?;
        return Ok(());
    }

    swap::protocol_v5::bob::redeem_xmr_with_wallet(
        database,
        swap_uuid,
        state5,
        monero_wallet,
        monero_receive_address,
        None,
        || {
            swap::util::call_basic_listener_method(
                env,
                swap_id.to_string(),
                swap::util::ON_START_REDEEM_XMR_SYNC_METHOD,
            )
        },
        || {
            swap::util::call_basic_listener_method(
                env,
                swap_id.to_string(),
                swap::util::ON_START_XMR_SWEEP_METHOD,
            )
        },
    )
    .await
    .map(|_| ())
}

/// Turns a raw error into one short, plain line for the swap log. Raw errors
/// from the Electrum client span several lines ("Made one or multiple attempts,
/// all errored:\n    - Resource temporarily unavailable (os error 35)") and read
/// like a stack trace. Known network causes get a plain phrase, boilerplate is
/// dropped, duplicates are merged, and the result is a single line. The full
/// error should still go to the log file (`tracing`) for diagnosis.
pub(crate) fn humanize_error(raw: &str) -> String {
    const MAX_LEN: usize = 160;

    fn plain_cause(line: &str) -> String {
        let lower = line.to_lowercase();
        if lower.contains("general socks server failure") {
            "Tor could not reach the server (circuit or DNS failure)".to_string()
        } else if lower.contains("os error 35")
            || lower.contains("os error 11")
            || lower.contains("resource temporarily unavailable")
            || lower.contains("timed out")
            || lower.contains("wall-clock deadline")
        {
            "the server did not answer in time".to_string()
        } else if lower.contains("connection refused")
            || lower.contains("connection reset")
            || lower.contains("broken pipe")
        {
            "the server refused or dropped the connection".to_string()
        } else {
            line.to_string()
        }
    }

    let mut causes: Vec<String> = Vec::new();
    for line in raw.lines() {
        let line = line.trim().trim_start_matches('-').trim();
        if line.is_empty() || line.starts_with("Made one or multiple attempts") {
            continue;
        }
        let cause = plain_cause(line);
        if !causes.contains(&cause) {
            causes.push(cause);
        }
    }
    if causes.is_empty() {
        return "unknown error".to_string();
    }

    let text = causes.join("; ");
    if text.chars().count() > MAX_LEN {
        let cut: String = text.chars().take(MAX_LEN - 1).collect();
        format!("{cut}…")
    } else {
        text
    }
}

/// Delay before retry number `attempt` (1-based) of a read that a funded swap
/// needs to keep going: 5 s, 10 s, 20 s, then 30 s.
pub(crate) fn locked_swap_retry_delay(attempt: u32) -> Duration {
    const BASE_SECS: u64 = 5;
    const CAP_SECS: u64 = 30;
    let doublings = attempt.saturating_sub(1).min(4);
    Duration::from_secs((BASE_SECS << doublings).min(CAP_SECS))
}

/// Runs `op` until it succeeds, for reads a funded swap cannot proceed without
/// (for example the cancel timelock). Once the Bitcoin lock is published a
/// transient failure (Electrum timeout, Tor stall) must never end the client:
/// it waits and retries. Returns `None` only when `keep_running` turns false,
/// i.e. the user stopped the swap.
pub(crate) async fn retry_while_locked<T, E, Fut, Op, Keep, Notify, Delay>(
    mut op: Op,
    mut keep_running: Keep,
    mut on_retry: Notify,
    delay: Delay,
) -> Option<T>
where
    Op: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, E>>,
    Keep: FnMut() -> bool,
    Notify: FnMut(u32, &E, Duration),
    Delay: Fn(u32) -> Duration,
{
    let mut attempt = 0u32;
    loop {
        match op().await {
            Ok(value) => return Some(value),
            Err(error) => {
                attempt += 1;
                if !keep_running() {
                    return None;
                }
                let wait = delay(attempt);
                on_retry(attempt, &error, wait);
                tokio::time::sleep(wait).await;
                if !keep_running() {
                    return None;
                }
            }
        }
    }
}

pub(crate) fn report_setup_error(env: &JNIEnv<'_>, swap_id: &str, message: String) {
    util::on_swap_error(
        env,
        SwapError {
            swap_id: swap_id.to_string(),
            error_type: ErrorType::SwapSetupFailed,
            error_message: message,
            fatal: true,
        },
    );
}

/// Reports a failure after the Bitcoin lock may be published. Unlike a setup error, the GUI
/// resumes the swap instead of ending it, because nothing else would watch the timelocks
/// (audit 2026-11-10, H-2).
pub(crate) fn report_post_lock_error(env: &JNIEnv<'_>, swap_id: &str, message: String) {
    util::on_swap_error(env, post_lock_error(swap_id, message));
}

/// Reports a failure of the cancel/refund path. A confirmed punish ends the swap as punished;
/// anything else resumes it (audit 2026-11-10, H-5 and H-2).
#[cfg(feature = "protocol-v5")]
pub(crate) fn report_cancel_refund_error(
    env: &JNIEnv<'_>,
    swap_id: &str,
    context: &str,
    error: &anyhow::Error,
) {
    util::on_swap_error(env, cancel_refund_error(swap_id, context, error));
}

/// What the app shows when a refund has finished.
#[cfg(feature = "protocol-v5")]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RefundReport {
    /// Shown as refunded; the line goes to the swap log.
    Refunded(String),
    /// Shown as partially refunded: (log line, sats back before fees, deposit sats lost).
    PartiallyRefunded(String, u64, u64),
}

#[cfg(feature = "protocol-v5")]
pub(crate) fn refund_report(outcome: &swap::protocol_v5::bob::RefundOutcome) -> RefundReport {
    use swap::protocol_v5::bob::RefundOutcome;
    match outcome {
        RefundOutcome::Refunded(txid) => {
            RefundReport::Refunded(format!("Bitcoin refund transaction confirmed: {}", txid))
        }
        RefundOutcome::DepositReturned(txid) => RefundReport::Refunded(format!(
            "Bitcoin refund confirmed and the seller's deposit claimed back ({}): all of your Bitcoin is back, minus fees.",
            txid
        )),
        RefundOutcome::DepositWithheld { withhold_txid, deposit, lock_amount } => {
            let refunded = lock_amount.to_sat().saturating_sub(deposit.to_sat());
            RefundReport::PartiallyRefunded(
                format!(
                    "Partially refunded: the seller kept its {} sat deposit (transaction {}). About {} sats of your {} sat lock came back, minus fees.",
                    deposit.to_sat(),
                    withhold_txid,
                    refunded,
                    lock_amount.to_sat()
                ),
                refunded,
                deposit.to_sat(),
            )
        }
    }
}

/// Reports a finished refund: refunded, or partially refunded when the seller kept the
/// deposit (audit H-1: a partial refund was always shown as refunded).
#[cfg(feature = "protocol-v5")]
pub(crate) fn report_refund_outcome(
    env: &JNIEnv<'_>,
    swap_id: &str,
    outcome: &swap::protocol_v5::bob::RefundOutcome,
) {
    match refund_report(outcome) {
        RefundReport::Refunded(line) => {
            util::print_swap_log_ln(env, line);
            util::call_basic_listener_method(env, swap_id.to_string(), util::ON_SWAP_REFUNDED_METHOD);
        }
        RefundReport::PartiallyRefunded(line, refunded, deposit) => {
            util::print_swap_log_ln(env, line);
            util::on_swap_partially_refunded(env, swap_id.to_string(), refunded, deposit);
        }
    }
}

#[cfg(feature = "protocol-v5")]
pub(crate) fn cancel_refund_error(swap_id: &str, context: &str, error: &anyhow::Error) -> SwapError {
    if let Some(punished) = error.downcast_ref::<swap::protocol_v5::bob::Punished>() {
        return SwapError {
            swap_id: swap_id.to_string(),
            error_type: ErrorType::SwapPunished,
            error_message: punished.to_string(),
            fatal: true,
        };
    }
    post_lock_error(swap_id, format!("{}: {}", context, error))
}

pub(crate) fn post_lock_error(swap_id: &str, message: String) -> SwapError {
    SwapError {
        swap_id: swap_id.to_string(),
        error_type: ErrorType::SwapInterrupted,
        error_message: message,
        fatal: true,
    }
}

#[cfg(feature = "protocol-v5")]
pub(crate) fn notify_protocol_v5_cancel(
    env: &JNIEnv<'_>,
    swap_id: &str,
    txid: String,
) {
    util::on_swap_canceled(env, swap_id.to_string(), txid);
}

#[cfg(feature = "protocol-v5")]
pub(crate) fn notify_protocol_v5_refund(env: &JNIEnv<'_>, swap_id: &str, txid: String) {
    util::on_swap_refund_broadcast(env, swap_id.to_string(), txid);
}

#[cfg(feature = "protocol-v5")]
pub(crate) fn notify_protocol_v5_cancel_prepared(env: &JNIEnv<'_>, swap_id: &str) {
    util::call_basic_listener_method(
        env,
        swap_id.to_string(),
        util::ON_SWAP_CANCEL_PREPARED_METHOD,
    );
}

pub(crate) fn map_swap_completion_error(error: anyhow::Error, swap_uuid: Uuid) -> SwapError {
    let error_string = error.to_string();
    let swap_error = if error_string == "Failed to deserialize bytes into message using CBOR" {
        SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::DeserializeCborError,
            error_message:
                "We had trouble deserializing message from peer using CBOR, retrying swap..."
                    .to_string(),
            fatal: true,
        }
    } else if error_string == "Failed to communicate encrypted signature through event loop channel"
    {
        SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::EncryptedSignatureTransferError,
            error_message: "Failed to properly communicate encrypted signature to peer. If they did not receive it, then the swap could fail and you will be refunded as long as the swap remains running. Restarting swap...".to_string(),
            fatal: true
        }
    } else if error_string == "Failed to receive encrypted signature ack through event loop channel"
    {
        SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::EncryptedSignatureAckError,
            error_message: "Failed to properly receive encrypted signature ack from peer. It is possible they received it, but we did not get acknowledgement of that. If they did not receive it, then the swap could fail and you will be refunded. Restarting swap...".to_string(),
            fatal: true
        }
    } else if error_string == "Seller encountered a problem, please try again later." {
        SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::SellerEncounteredProblemError,
            error_message: "Seller encountered an unknown problem. Retrying swap...".to_string(),
            fatal: true,
        }
    } else if error_string == "Failed to receive transfer proof" {
        SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::FailedToReceiveTransferProof,
            error_message: "Failed to receive transfer proof. Restarting swap...".to_string(),
            fatal: true,
        }
    } else if error_string
        == "Electrum client error: Electrum server error: \"missing transaction\""
    {
        SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::ElectrumMissingTransaction,
            error_message: error_string,
            fatal: true,
        }
    } else if error_string == "Failed to subscribe to header notifications 1" {
        SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::ElectrumFailedToSubscribeToHeaders,
            error_message: error_string,
            fatal: true,
        }
    } else if let Ok(_error) = error.downcast::<SwapDisconnected>() {
        SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::SwapDisconnected,
            error_message: error_string,
            fatal: true,
        }
    } else if error_string.starts_with("Failed to broadcast Bitcoin refund transaction") {
        // The refund spends the cancel transaction's output; broadcasting it
        // too soon after the cancel can race the Electrum propagation and
        // fail. The client restarts, resumes from BtcCancelled, and retries
        // the refund after the cancel confirms — recovery is automatic.
        SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::RefundBroadcastFailed,
            error_message: "Refund broadcast failed (it raced the cancel transaction's propagation). Restarting the swap client automatically; the BTC refund will be published after the cancel transaction confirms. No action needed — keep the client running.".to_string(),
            fatal: true
        }
    } else if error_string.starts_with("Failed to setup swap after retries") {
        // The swap setup (spot price negotiation) never completed after all
        // retries - the seller is unreachable, broken, or running incompatible
        // software. Nothing was locked, so there is nothing to recover; the
        // user should pick a different seller. No automatic restart: retrying
        // the same seller would just burn time.
        SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::SwapSetupFailed,
            error_message: "Swap setup failed: the seller did not respond to the swap setup (spot price) request after repeated retries. The seller may be offline, broken, or running incompatible software. No funds were locked or spent. Select a different seller and try again.".to_string(),
            fatal: false
        }
    } else {
        SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::UnknownError,
            error_message: error_string,
            fatal: true,
        }
    };
    return swap_error;
}
pub(crate) fn slice_to_array_32<T>(slice: &[T]) -> Result<&[T; 32], TryFromSliceError> {
    if slice.len() == 32 {
        let ptr = slice.as_ptr() as *const [T; 32];
        unsafe { Ok(&*ptr) }
    } else {
        Err(TryFromSliceError(()))
    }
}

#[tokio::main]
#[no_mangle]
#[warn(unused_variables)]
pub async extern "system" fn Java_swap_helper_HelperAddress_isValidXmrAddress(
    env: JNIEnv,
    _class: JClass,
    address_jstring: jstring,
) -> jboolean {
    if let Err(_e) = util::jstring_to_xmr_address(&env, address_jstring) {
        0
    } else {
        1
    }
}

#[tokio::main]
#[no_mangle]
#[warn(unused_variables)]
pub async extern "system" fn Java_swap_helper_HelperAddress_isValidLibP2pAddress(
    env: JNIEnv,
    _class: JClass,
    address_jstring: jstring,
) -> jboolean {
    if let Err(_e) = util::jstring_to_libp2p_multiaddr(&env, address_jstring) {
        0
    } else {
        1
    }
}

#[tokio::main]
#[no_mangle]
#[warn(unused_variables)]
pub async extern "system" fn Java_swap_lib_AppXmrRpc_maybeDownloadXmrRpc(
    env: JNIEnv,
    _class: JClass,
    proxy_jstring: jstring,
) -> jstring {
    let data_dir = match fs::system_data_dir() {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(
                ?error,
                "Failed to resolve the data directory for Monero RPC"
            );
            return download_rpc_result_to_jstring(
                &env,
                DownloadRpcResult {
                    rpc_path: "".to_string(),
                    error: format!("Failed to resolve the data directory: {error}"),
                },
            );
        }
    };
    if data_dir.exists().not() {
        if let Err(error) = std::fs::create_dir_all(data_dir.as_path()) {
            tracing::error!(?error, "Failed to create the Monero RPC data directory");
            return download_rpc_result_to_jstring(
                &env,
                DownloadRpcResult {
                    rpc_path: "".to_string(),
                    error: format!("Failed to create the data directory: {error}"),
                },
            );
        }
    }
    let proxy = match util::get_string_value(&env, proxy_jstring) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to read the Monero RPC proxy setting");
            return download_rpc_result_to_jstring(
                &env,
                DownloadRpcResult {
                    rpc_path: "".to_string(),
                    error: "Failed to read the proxy setting.".to_string(),
                },
            );
        }
    };
    let rpc_result = util::maybe_download_xmr_rpc(&env, data_dir.clone(), proxy).await;
    let result = match rpc_result {
        Ok(_rpc) => DownloadRpcResult {
            rpc_path: data_dir.join("monero").to_string_lossy().into_owned(),
            error: "".to_string(),
        },
        Err(error) => DownloadRpcResult {
            rpc_path: "".to_string(),
            error: error.to_string(),
        },
    };
    download_rpc_result_to_jstring(&env, result)
}

fn download_rpc_result_to_jstring(env: &JNIEnv<'_>, result: DownloadRpcResult) -> jstring {
    match serde_json::to_string(&result) {
        Ok(response_json) => match env.new_string(response_json) {
            Ok(response) => response.into_inner(),
            Err(error) => {
                tracing::error!(?error, "Failed to allocate the Monero RPC response string");
                std::ptr::null_mut()
            }
        },
        Err(error) => {
            tracing::error!(?error, "Failed to serialize the Monero RPC response");
            std::ptr::null_mut()
        }
    }
}

#[tokio::main]
#[no_mangle]
#[warn(unused_variables)]
pub async extern "system" fn Java_swap_lib_AppSwap_getDataDir(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let data_dir = match fs::system_data_dir() {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to resolve the system data directory");
            return match env.new_string("") {
                Ok(response) => response.into_inner(),
                Err(error) => {
                    tracing::error!(?error, "Failed to allocate the data directory string");
                    std::ptr::null_mut()
                }
            };
        }
    };
    match env.new_string(data_dir.to_string_lossy().into_owned()) {
        Ok(response) => response.into_inner(),
        Err(error) => {
            tracing::error!(?error, "Failed to allocate the data directory string");
            std::ptr::null_mut()
        }
    }
}

pub fn get_sys_time_in_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

pub(crate) async fn wait_for_swap_client_kill(env: &JNIEnv<'_>) -> Result<()> {
    while util::get_running_swap(&env) {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    util::print_swap_log_ln(&env, format!("Killed!"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_seed_accepts_a_64_byte_base64_seed() {
        let encoded = base64::encode([7u8; 64]);
        assert!(decode_seed(&encoded).is_ok());
    }

    #[test]
    fn decode_seed_rejects_wrong_length_and_invalid_base64() {
        assert!(decode_seed(&base64::encode([7u8; 32])).is_err());
        assert!(decode_seed("not base64!").is_err());
    }

    // Swap f7a162f7 (2026-11-06): the cancel-timelock read failed with an
    // Electrum EAGAIN, the client reported a fatal error and stopped while the
    // BTC lock and Alice's XMR lock were both on-chain.
    #[tokio::test]
    async fn retry_while_locked_survives_transient_failures() {
        let calls = std::cell::Cell::new(0u32);
        let retried = std::cell::Cell::new(0u32);
        let value = retry_while_locked(
            || {
                calls.set(calls.get() + 1);
                let attempt = calls.get();
                async move {
                    if attempt < 3 {
                        Err("Resource temporarily unavailable (os error 35)")
                    } else {
                        Ok(42)
                    }
                }
            },
            || true,
            |_, _, _| retried.set(retried.get() + 1),
            |_| Duration::ZERO,
        )
        .await;

        assert_eq!(value, Some(42));
        assert_eq!(calls.get(), 3);
        assert_eq!(retried.get(), 2);
    }

    #[tokio::test]
    async fn retry_while_locked_stops_when_the_user_stops_the_swap() {
        let calls = std::cell::Cell::new(0u32);
        let value: Option<u32> = retry_while_locked(
            || {
                calls.set(calls.get() + 1);
                async { Err::<u32, _>("still failing") }
            },
            || calls.get() < 2,
            |_, _, _| {},
            |_| Duration::ZERO,
        )
        .await;

        assert_eq!(value, None);
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn humanize_error_collapses_the_electrum_multi_line_error() {
        // Verbatim from swap bf5f78fa's log (2026-11-07).
        let raw = "Made one or multiple attempts, all errored:\n\t- Resource temporarily unavailable (os error 35)\n";
        let human = humanize_error(raw);
        assert_eq!(human, "the server did not answer in time");
        assert!(!human.contains('\n'));
    }

    #[test]
    fn humanize_error_explains_a_tor_socks_failure() {
        assert_eq!(
            humanize_error("general SOCKS server failure (Most likely DNS resolution error)"),
            "Tor could not reach the server (circuit or DNS failure)"
        );
    }

    #[test]
    fn humanize_error_merges_duplicates_and_keeps_distinct_causes() {
        let raw = "Made one or multiple attempts, all errored:\n- Resource temporarily unavailable (os error 35)\n- Resource temporarily unavailable (os error 35)\n- Connection refused (os error 61)";
        assert_eq!(
            humanize_error(raw),
            "the server did not answer in time; the server refused or dropped the connection"
        );
    }

    #[test]
    fn humanize_error_keeps_unknown_errors_but_bounds_them_to_one_line() {
        assert_eq!(humanize_error("  weird failure  "), "weird failure");
        assert_eq!(humanize_error(""), "unknown error");
        assert_eq!(humanize_error("Made one or multiple attempts, all errored:\n"), "unknown error");
        let long = format!("odd\n{}", "x".repeat(400));
        let human = humanize_error(&long);
        assert!(!human.contains('\n'));
        assert!(human.chars().count() <= 160, "{}", human.chars().count());
        assert!(human.ends_with('…'));
    }

    #[test]
    fn locked_swap_retry_delay_backs_off_and_is_capped() {
        let secs: Vec<u64> = (1..=8)
            .map(|attempt| locked_swap_retry_delay(attempt).as_secs())
            .collect();
        assert_eq!(secs, vec![5, 10, 20, 30, 30, 30, 30, 30]);
        assert_eq!(locked_swap_retry_delay(0).as_secs(), 5);
        assert_eq!(locked_swap_retry_delay(u32::MAX).as_secs(), 30);
    }
}

#[cfg(test)]
mod sweep_prepare_retry_tests {
    use super::*;

    #[test]
    fn a_daemon_decode_error_is_retried() {
        assert!(!sweep_prepare_error_is_permanent(
            "Monero sweep construction failed: Decoy selection error: interface error (interface error (error decoding response body))"
        ));
    }

    #[test]
    fn invalid_keys_or_address_are_not_retried() {
        assert!(sweep_prepare_error_is_permanent("invalid recovered XMR spend key: bad"));
        assert!(sweep_prepare_error_is_permanent("invalid XMR redeem address: bad"));
    }

    #[test]
    fn the_delay_grows_then_is_capped() {
        let delays: Vec<u64> = (2..=8).map(sweep_prepare_retry_delay_secs).collect();
        assert_eq!(delays, vec![5, 10, 20, 40, 60, 60, 60]);
    }
}

#[cfg(test)]
mod post_lock_error_tests {
    use super::post_lock_error;

    #[test]
    fn a_post_lock_failure_is_reported_as_interrupted_not_as_setup_failed() {
        let error = post_lock_error("abc", "Failed to cancel/refund Bitcoin: timeout".to_string());
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["errorType"], "SwapInterrupted");
        assert_eq!(json["swapId"], "abc");
        assert_eq!(json["fatal"], true);
        assert_eq!(json["errorMessage"], "Failed to cancel/refund Bitcoin: timeout");
    }

    /// The GUI parses the error type by name (SwapErrorType.java). A name missing there makes
    /// the error unreadable, so the two lists must stay the same.
    #[test]
    fn every_error_type_is_known_to_the_gui() {
        let rust = include_str!("../xmr-btc-swap/swap/src/swap_error.rs");
        let java = include_str!("../app/src/main/java/swap/model/SwapErrorType.java");
        let names = |text: &str, start: &str| -> Vec<String> {
            let body = &text[text.find(start).unwrap()..];
            let body = &body[body.find('{').unwrap() + 1..body.find('}').unwrap()];
            body.lines()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty() && !line.starts_with("//") && !line.starts_with('*') && !line.starts_with("/*"))
                .map(|line| line.trim_end_matches([',', ';']).to_string())
                .filter(|name| name.chars().all(|c| c.is_ascii_alphanumeric()))
                .collect()
        };
        let rust_names = names(rust, "pub enum  ErrorType");
        let java_names = names(java, "public enum SwapErrorType");
        assert!(rust_names.contains(&"SwapInterrupted".to_string()));
        assert_eq!(rust_names, java_names);
    }

    /// Which reporter each message goes through: the call just before the message text.
    fn reporter_of(source: &str, message: &str) -> Vec<&'static str> {
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(at) = source[from..].find(message) {
            let at = from + at;
            let before = &source[..at];
            let post = before
                .rfind("report_post_lock_error(")
                .max(before.rfind("report_cancel_refund_error("));
            let setup = before.rfind("report_setup_error(");
            found.push(match (post, setup) {
                (Some(p), Some(s)) if p > s => "post_lock",
                (Some(_), None) => "post_lock",
                _ => "setup",
            });
            from = at + message.len();
        }
        found
    }

    const BUY: &str = include_str!("buy_xmr_v5.rs");
    const RESUME: &str = include_str!("resume_v5.rs");

    /// Every failure that can happen once the Bitcoin lock may be out resumes the swap.
    #[test]
    fn failures_after_the_lock_resume_the_swap() {
        let post_lock = [
            "Failed to claim back the Bitcoin deposit",
            "Failed to publish Bitcoin lock",
            "Failed to republish Bitcoin lock",
            "Failed to persist the Bitcoin cancel path",
            "\"Failed to cancel/refund Bitcoin\"",
            "\"Failed to finish cancel/refund\"",
            "Failed to redeem XMR",
            "Failed to wait for Monero lock finality",
            "Failed to resend redeem signature",
            "Failed to verify persisted Monero lock proof",
            "Bitcoin early refund is published but not confirmed",
            "Bitcoin refund transaction is published but not confirmed",
            // audit H-3: the early refund gave way to the cancel timelock
            "did not confirm before the cancel timelock; switching to cancel/refund",
        ];
        for message in post_lock {
            let reporters: Vec<_> = [reporter_of(BUY, message), reporter_of(RESUME, message)].concat();
            assert!(!reporters.is_empty(), "{} is not reported anywhere", message);
            if message.contains("switching to cancel/refund") {
                assert_eq!(reporters.len(), 3, "buy path and both resume paths");
            }
            assert!(reporters.iter().all(|r| *r == "post_lock"), "{}: {:?}", message, reporters);
        }
    }

    /// Failures before the lock, and states no retry can fix, still end the swap.
    #[test]
    fn setup_failures_and_permanent_states_still_end_the_swap() {
        for message in [
            "Invalid swap id",
            "Protocol-5 setup failed",
            "Protocol-5 commit failed",
            "Failed to prepare Bitcoin lock",
        ] {
            let reporters = reporter_of(BUY, message);
            assert!(!reporters.is_empty(), "{}", message);
            assert!(reporters.iter().all(|r| *r == "setup"), "{}: {:?}", message, reporters);
        }
        for message in [
            "No protocol-5 state exists for this swap.",
            "Persisted Monero lock proof does not verify.",
            "is not resumable yet.",
        ] {
            let reporters = reporter_of(RESUME, message);
            assert!(!reporters.is_empty(), "{}", message);
            assert!(reporters.iter().all(|r| *r == "setup"), "{}: {:?}", message, reporters);
        }
    }
}

#[cfg(all(test, feature = "protocol-v5"))]
mod refund_report_tests {
    use super::{refund_report, RefundReport};
    use swap::protocol_v5::bob::RefundOutcome;

    fn txid() -> swap::bitcoin::Txid {
        "0000000000000000000000000000000000000000000000000000000000000003".parse().unwrap()
    }

    #[test]
    fn a_full_refund_is_shown_as_refunded() {
        assert!(matches!(refund_report(&RefundOutcome::Refunded(txid())), RefundReport::Refunded(_)));
    }

    #[test]
    fn a_returned_deposit_is_shown_as_refunded() {
        match refund_report(&RefundOutcome::DepositReturned(txid())) {
            RefundReport::Refunded(line) => assert!(line.contains("deposit claimed back"), "{}", line),
            other => panic!("{:?}", other),
        }
    }

    /// Before: a partial refund was shown as "refunded" whatever happened to the deposit.
    #[test]
    fn a_kept_deposit_is_shown_as_partially_refunded_with_the_amounts() {
        let report = refund_report(&RefundOutcome::DepositWithheld {
            withhold_txid: txid(),
            deposit: swap::bitcoin::Amount::from_sat(10_000),
            lock_amount: swap::bitcoin::Amount::from_sat(500_000),
        });
        match report {
            RefundReport::PartiallyRefunded(line, refunded, deposit) => {
                assert_eq!((refunded, deposit), (490_000, 10_000));
                assert!(line.contains("kept its 10000 sat deposit"), "{}", line);
            }
            other => panic!("{:?}", other),
        }
    }

    /// Every refund call site goes through the reporter, so none can say "refunded" for a
    /// kept deposit (source check: the JNI functions cannot run without the JVM).
    #[test]
    fn every_refund_result_goes_through_the_reporter() {
        for (name, source) in [
            ("buy_xmr_v5.rs", include_str!("buy_xmr_v5.rs")),
            ("resume_v5.rs", include_str!("resume_v5.rs")),
        ] {
            for call in ["bob::run_cancel_and_refund(", "bob::wait_for_refund_confirmation(", "bob::reclaim_deposit("] {
                for (at, _) in source.match_indices(call) {
                    let after = &source[at..];
                    let arm = &after[after.find("Ok(").unwrap()..];
                    let arm = &arm[..arm.find("Err(").unwrap()];
                    assert!(
                        arm.contains("report_refund_outcome("),
                        "{} {} at byte {}: {}",
                        name,
                        call,
                        at,
                        arm
                    );
                }
            }
        }
    }
}

#[cfg(all(test, feature = "protocol-v5"))]
mod cancel_refund_error_tests {
    //! Audit 2026-11-10, H-5: a real punish ends the swap; every other refund failure resumes.
    use super::cancel_refund_error;
    use swap::protocol_v5::bob::Punished;

    fn txid() -> swap::bitcoin::Txid {
        "0000000000000000000000000000000000000000000000000000000000000002".parse().unwrap()
    }

    fn json(error: &swap::swap_error::SwapError) -> serde_json::Value {
        serde_json::to_value(error).unwrap()
    }

    #[test]
    fn a_confirmed_punish_is_reported_as_punished() {
        let error = anyhow::Error::from(Punished(txid()));
        let reported = json(&cancel_refund_error("s", "Failed to cancel/refund Bitcoin", &error));
        assert_eq!(reported["errorType"], "SwapPunished");
        assert!(reported["errorMessage"].as_str().unwrap().contains("punish transaction"));
    }

    #[test]
    fn a_punish_wrapped_in_context_is_still_recognised() {
        let error = anyhow::Error::from(Punished(txid())).context("refund loop");
        let reported = json(&cancel_refund_error("s", "Failed to finish cancel/refund", &error));
        assert_eq!(reported["errorType"], "SwapPunished");
    }

    #[test]
    fn any_other_refund_failure_resumes_the_swap() {
        let error = anyhow::anyhow!("Electrum: all servers failed");
        let reported = json(&cancel_refund_error("s", "Failed to cancel/refund Bitcoin", &error));
        assert_eq!(reported["errorType"], "SwapInterrupted");
        assert_eq!(
            reported["errorMessage"],
            "Failed to cancel/refund Bitcoin: Electrum: all servers failed"
        );
    }
}

#[cfg(test)]
mod xmr_received_report_tests {
    /// Matrix N-15: both daemon sweep paths (a stored sweep sent again, a new sweep) tell the GUI
    /// the amount through the same helper, and never call the bare txid callback directly
    /// (source check: the JNI side needs the JVM to run).
    #[test]
    fn both_sweep_paths_report_the_amount() {
        let source = include_str!("util.rs");
        let start = source.find("pub(crate) async fn redeem_xmr(").unwrap();
        let body = &source[start..];
        let body = &body[..body.find("swap::protocol_v5::bob::redeem_xmr_with_wallet(").unwrap()];
        assert_eq!(body.matches("report_sweep_broadcast(env, swap_id, txid,").count(), 2);
        assert!(!body.contains("on_xmr_sweep_broadcast("));
        assert!(body.contains("bob::stored_sweep(database, swap_uuid)"));
        assert!(body.contains("received_or_log(state5, &sweep)"));
    }
}
