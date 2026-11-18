use std::future::Future;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use crate::protocol::alice::swap::{
    redeem_transaction_is_known, retry_until_cancel, RetryOutcome, REDEEM_RETRY_INITIAL_INTERVAL,
    REDEEM_RETRY_MAX_INTERVAL,
};
use crate::protocol_v5::core::bitcoin::ExpiredTimelocks;
use uuid::Uuid;

use crate::protocol_v5::core::monero::BlockHeight;
use crate::protocol_v5::db;
use crate::protocol_v5::event_loop::{AliceSwapHandle, MakerEventLoopHandle, RegisteredSwap};
use crate::protocol_v5::machine::alice::AliceState;
use crate::protocol_v5::store::{PendingMoneroLock, ProtocolV5Store};

pub trait LiquidityRelease: Send + Sync {
    fn release(&self, swap_id: Uuid);
}

#[derive(Clone)]
pub struct AliceContext {
    pub store: Arc<dyn ProtocolV5Store>,
    pub bitcoin_wallet: Arc<crate::bitcoin::Wallet>,
    pub monero_wallet: Arc<crate::monero::Wallet>,
    pub monero_daemon: Option<Arc<crate::monero::ng::Daemon>>,
    pub env_config: crate::protocol_v5::env::env::Config,
    pub monero_network: ::monero::Network,
    pub liquidity: Option<Arc<dyn LiquidityRelease>>,
}

pub async fn run_maker(
    mut event_loop: MakerEventLoopHandle,
    context: AliceContext,
) -> Result<()> {
    loop {
        let swap = event_loop.next_swap().await?;
        let swap_id = swap.swap_id;
        let context = context.clone();
        let liquidity = context.liquidity.clone();
        tokio::spawn(async move {
            if let Err(error) = run(context, swap).await {
                tracing::info!(%swap_id, error = %format!("{error:#}"), "ASB_SWAP_FAIL");
                if let Some(liquidity) = liquidity {
                    liquidity.release(swap_id);
                }
            }
        });
    }
}

/// How often a waiting punish transaction is re-ensured on the network.
const PUNISH_REBROADCAST_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// How often a waiting refund sweep is republished.
const REFUND_REBROADCAST_INTERVAL: Duration = Duration::from_secs(5 * 60);

async fn wait_for_cancel_timelock(
    state3: &crate::protocol_v5::machine::alice::State3,
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
) -> Result<()> {
    loop {
        if !matches!(
            state3.expired_timelocks(bitcoin_wallet).await?,
            ExpiredTimelocks::None { .. }
        ) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// How long to pause when a driver step returned the state it started from
/// almost at once. A step that waited (a confirmation, a retry) takes seconds, so
/// only an immediate identical return is a spin.
fn no_progress_delay(before: &str, after: &str, elapsed: Duration) -> Option<Duration> {
    if before == after && elapsed < Duration::from_secs(2) {
        Some(Duration::from_secs(5))
    } else {
        None
    }
}

/// Runs `work` until the cancel timelock expires. `Ok(None)` means the timelock
/// won, so the caller must take the cancel path instead of waiting on.
///
/// The wait for Alice's Monero lock to appear used to run alone: if the lock
/// transaction never showed up, Alice sat there (up to 30 minutes) after Bob had
/// already cancelled and refunded (audit A-4).
async fn until_cancel_timelock<T, W, C>(work: W, cancel_timelock: C) -> Result<Option<T>>
where
    W: std::future::Future<Output = Result<T>>,
    C: std::future::Future<Output = Result<()>>,
{
    tokio::select! {
        result = work => result.map(Some),
        expired = cancel_timelock => {
            expired?;
            Ok(None)
        }
    }
}

/// Waits for a confirmation, republishing the transaction every `interval`.
///
/// A refund sweep that the pool dropped used to be waited on for the whole
/// 30-minute confirmation deadline before the swap retried and republished it
/// (audit A-12).
async fn wait_rebroadcasting<W, WF, R, RF>(
    mut wait: W,
    mut rebroadcast: R,
    interval: Duration,
) -> Result<()>
where
    W: FnMut() -> WF,
    WF: std::future::Future<Output = Result<()>>,
    R: FnMut() -> RF,
    RF: std::future::Future<Output = ()>,
{
    loop {
        match tokio::time::timeout(interval, wait()).await {
            Ok(result) => return result,
            Err(_) => rebroadcast().await,
        }
    }
}

/// An Alice state this driver has no arm for. Retrying cannot help, so this is
/// the one error that ends a maker swap task.
#[derive(Debug, thiserror::Error)]
#[error("Unsupported Alice state for this driver: {0}")]
pub(crate) struct UnsupportedAliceState(pub String);

/// Whether an error from a driver step is permanent. Everything else (Electrum,
/// wallet-RPC, Tor, DB hiccups) is transient and must be retried: until now any
/// error ended the swap task, so a single Electrum blip could leave a funded swap
/// unwatched until the ASB was restarted (audit finding A-1).
pub(crate) fn is_fatal_swap_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<UnsupportedAliceState>().is_some()
        || error
            .chain()
            .any(|cause| cause.to_string().contains("maker event loop stopped"))
}

/// Delay before retry number `attempt` (1-based) of a failed maker step:
/// 5 s, 10 s, 20 s, 40 s, 80 s, then 120 s.
pub(crate) fn maker_retry_delay(attempt: u32) -> Duration {
    const BASE_SECS: u64 = 5;
    const CAP_SECS: u64 = 120;
    let doublings = attempt.saturating_sub(1).min(5);
    Duration::from_secs((BASE_SECS << doublings).min(CAP_SECS))
}

/// How long to wait before retrying after `error` (`attempt` is 1-based), or
/// `None` when the error is fatal and the swap task must end.
pub(crate) fn retry_delay_for(error: &anyhow::Error, attempt: u32) -> Option<Duration> {
    if is_fatal_swap_error(error) {
        None
    } else {
        Some(maker_retry_delay(attempt))
    }
}

/// Runs `operation` until it succeeds, retrying transient errors with
/// `delay(attempt)` between attempts; a fatal error is returned immediately.
pub(crate) async fn retry_unless_fatal<T, Op, Fut>(
    mut operation: Op,
    delay: impl Fn(u32) -> Duration,
    mut on_retry: impl FnMut(u32, &anyhow::Error, Duration),
) -> Result<T>
where
    Op: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut attempt = 0u32;
    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error) if is_fatal_swap_error(&error) => return Err(error),
            Err(error) => {
                attempt += 1;
                let wait = delay(attempt);
                on_retry(attempt, &error, wait);
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// Where an `EncSigLearned` swap goes next.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EncSigRoute {
    /// The cancel timelock has not expired: broadcast the redeem.
    AttemptRedeem,
    /// The redeem is already on the network: wait for it to confirm.
    RedeemAlreadyPublished,
    /// The cancel timelock expired and the redeem is not known: cancel/refund.
    CancelRefund,
}

/// `redeem_known` only matters once the cancel timelock has expired: a restart
/// may resume here after the redeem was broadcast but before the state change
/// was persisted, so look for the deterministic redeem tx before cancelling.
pub(crate) fn encsig_route(timelock_open: bool, redeem_known: bool) -> EncSigRoute {
    if timelock_open {
        EncSigRoute::AttemptRedeem
    } else if redeem_known {
        EncSigRoute::RedeemAlreadyPublished
    } else {
        EncSigRoute::CancelRefund
    }
}

pub async fn run(mut context: AliceContext, mut swap: RegisteredSwap) -> Result<AliceState> {
    let initial = AliceState::Started {
        state3: Box::new(swap.state3.clone()),
    };
    run_from(context, swap, initial).await
}

pub async fn run_from(
    mut context: AliceContext,
    mut swap: RegisteredSwap,
    mut state: AliceState,
) -> Result<AliceState> {
    let swap_id = swap.swap_id;
    tracing::info!(%swap_id, "ASB_SWAP_STARTED");

    loop {
        if crate::protocol_v5::machine::alice::is_complete(&state) {
            context
                .store
                .store_alice(swap_id, &db::alice::Alice::from(state.clone()))
                .await?;
            context.store.release_lock_inputs(swap_id).await?;
            if let Some(liquidity) = &context.liquidity {
                liquidity.release(swap_id);
            }
            tracing::info!(%swap_id, state = %state, "ASB_SWAP_COMPLETE");
            return Ok(state);
        }

        let before = state.to_string();
        let step_started = std::time::Instant::now();
        // Transient errors are retried in place; only a fatal one ends the task
        // (and releases the liquidity reservation in the caller).
        let next = {
            let mut attempt = 0u32;
            loop {
                match advance(&context, swap.handle(), swap_id, state.clone()).await {
                    Ok(next) => break next,
                    Err(error) => {
                        attempt += 1;
                        let Some(wait) = retry_delay_for(&error, attempt) else {
                            return Err(error);
                        };
                        tracing::warn!(
                            %swap_id,
                            state = %state,
                            attempt,
                            retry_in_secs = wait.as_secs(),
                            "ASB_SWAP_RETRY: {error:#}"
                        );
                        tokio::time::sleep(wait).await;
                    }
                }
            }
        };
        // A state that comes straight back unchanged would otherwise spin, writing
        // the same row to the database on every pass (audit A-11).
        if let Some(delay) = no_progress_delay(&before, &next.to_string(), step_started.elapsed()) {
            tracing::warn!(%swap_id, state = %next, "ASB_SWAP_NO_PROGRESS: the same state came back immediately; pausing");
            tokio::time::sleep(delay).await;
        }
        state = next;
        // A failing database write must not drop the transition either.
        retry_unless_fatal(
            || async {
                context
                    .store
                    .store_alice(swap_id, &db::alice::Alice::from(state.clone()))
                    .await
            },
            maker_retry_delay,
            |attempt, error, wait| {
                tracing::warn!(
                    %swap_id,
                    attempt,
                    retry_in_secs = wait.as_secs(),
                    "ASB_SWAP_STORE_RETRY: {error:#}"
                );
            },
        )
        .await?;
        // Bob's encrypted signature is acknowledged only now that the state it
        // produced is stored: he never resends after an ack (audit A-3).
        match swap.handle().ack_encrypted_signature() {
            Ok(true) => tracing::info!(%swap_id, "ASB_ENCSIG_ACKED"),
            Ok(false) => {}
            Err(error) => tracing::warn!(%swap_id, "{error:#}"),
        }
    }
}

/// Alice must not publish the Monero lock when the Bitcoin cancel timelock is
/// within the confirmation upper bound: the lock can take long enough that the
/// timelock expires before Bob can redeem.
fn timelock_window_is_safe(expired: &ExpiredTimelocks, margin: u32) -> bool {
    matches!(
        expired,
        ExpiredTimelocks::None { blocks_left } if *blocks_left > margin
    )
}

async fn advance(
    context: &AliceContext,
    handle: &mut AliceSwapHandle,
    swap_id: Uuid,
    state: AliceState,
) -> Result<AliceState> {
    match state {
        AliceState::Started { state3 } => {
            let subscription = context
                .bitcoin_wallet
                .subscribe_to(state3.tx_lock.clone())
                .await;
            match tokio::time::timeout(
                context.env_config.bitcoin_lock_mempool_timeout,
                subscription.wait_until_seen(),
            )
            .await
            {
                Err(_) => Ok(AliceState::SafelyAborted),
                Ok(result) => {
                    result.context("Failed to watch Bob's Bitcoin lock")?;
                    tracing::info!(%swap_id, "ASB_BTC_LOCK_SEEN");
                    Ok(AliceState::BtcLockTransactionSeen { state3 })
                }
            }
        }
        AliceState::BtcLockTransactionSeen { state3 } => {
            let subscription = context
                .bitcoin_wallet
                .subscribe_to(state3.tx_lock.clone())
                .await;
            match tokio::time::timeout(
                context.env_config.bitcoin_lock_confirmed_timeout,
                subscription.wait_until_final(),
            )
            .await
            {
                Err(_) => Ok(AliceState::BtcEarlyRefundable { state3 }),
                Ok(result) => {
                    result.context("Failed to wait for Bob's Bitcoin lock finality")?;
                    tracing::info!(%swap_id, "ASB_BTC_LOCK");
                    Ok(AliceState::BtcLocked { state3 })
                }
            }
        }
        AliceState::BtcLocked { state3 } => {
            if !matches!(
                state3.expired_timelocks(&**context.bitcoin_wallet).await?,
                ExpiredTimelocks::None { .. }
            ) {
                return Ok(AliceState::WaitingForCancelTimelockExpiration {
                    monero_wallet_restore_blockheight: BlockHeight { height: 0 },
                    transfer_proof: None,
                    state3,
                });
            }
            let height = context
                .monero_wallet
                .block_height()
                .await
                .context("Failed to read Monero restore height")?;
            Ok(AliceState::XmrReadyToLock {
                state3,
                monero_wallet_restore_blockheight: BlockHeight {
                    height: height.height as u64,
                },
            })
        }
        AliceState::XmrReadyToLock {
            state3,
            monero_wallet_restore_blockheight,
        } => {
            let expired = state3.expired_timelocks(&**context.bitcoin_wallet).await?;
            if !timelock_window_is_safe(
                &expired,
                context
                    .env_config
                    .bitcoin_blocks_till_confirmed_upper_bound_assumption,
            ) {
                return Ok(AliceState::WaitingForCancelTimelockExpiration {
                    monero_wallet_restore_blockheight,
                    transfer_proof: None,
                    state3,
                });
            }
            if let ExpiredTimelocks::None { blocks_left } = expired {
                tracing::debug!(
                    %swap_id,
                    blocks_left,
                    "ASB_XMR_LOCK_TIMELOCK_WINDOW"
                );
            }

            if let Some(daemon) = &context.monero_daemon {
                let transfer_request = state3.lock_xmr_transfer_request();
                let public_spend_key =
                    ::monero::PublicKey::from_slice(&transfer_request.public_spend_key.as_bytes())
                        .context("Invalid protocol Monero spend key")?;
                let private_view_key = crate::monero::PrivateViewKey::from_private_key(
                    ::monero::PrivateKey::from_slice(&state3.v.0.as_bytes())
                        .context("Invalid protocol Monero view key")?,
                );
                // The wallet can briefly report a height one block ahead of
                // the daemon. Clamp the scan start so that race cannot turn
                // the shared-wallet safety check into a swap failure.
                let daemon_tip = daemon.chain_height().await?;
                let scan_start = monero_wallet_restore_blockheight.height.min(daemon_tip);
                let received = daemon
                    .scan_received_outputs(
                        &public_spend_key,
                        &private_view_key,
                        scan_start,
                        Some(daemon_tip),
                    )
                    .await?;
                if received {
                    bail!("The shared Monero address already received output; refusing to lock XMR");
                }
            } else {
                tracing::warn!(
                    %swap_id,
                    "No Monero daemon configured; skipping the shared-wallet output scan"
                );
            }

            let constructed = if let Some(pending) = context
                .store
                .load_pending_monero_lock(swap_id)
                .await?
            {
                crate::monero::wallet::ConstructedTx {
                    tx_hash: crate::monero::TxHash(pending.tx_hash),
                    tx_blob: pending.tx_blob,
                    tx_metadata: pending.tx_metadata,
                    tx_key: Some(
                        ::monero::PrivateKey::from_str(&pending.tx_key)
                            .context("Invalid stored Monero transaction key")?,
                    ),
                    amount: crate::monero::Amount::from_piconero(state3.xmr.as_pico()),
                    fee: crate::monero::Amount::from_piconero(0),
                }
            } else {
                let transfer_request = state3.lock_xmr_transfer_request();
                let (address, _) =
                    transfer_request.address_and_amount(context.env_config.monero_network);
                let address = crate::monero::Address::from_str(&address.to_string())
                    .context("Failed to convert the protocol Monero lock address")?;
                let amount = crate::monero::Amount::from_piconero(state3.xmr.as_pico());
                let constructed = context
                    .monero_wallet
                    .construct_transfer(vec![(address, amount)])
                    .await
                    .context("Failed to construct the Monero lock transaction")?;
                let proof = constructed.transfer_proof()?;
                context
                    .store
                    .store_pending_monero_lock(
                        swap_id,
                        &PendingMoneroLock {
                            tx_hash: constructed.tx_hash.0.clone(),
                            tx_blob: constructed.tx_blob.clone(),
                            tx_metadata: constructed.tx_metadata.clone(),
                            tx_key: proof.tx_key().to_string(),
                        },
                    )
                    .await?;
                constructed
            };

            if !timelock_window_is_safe(
                &state3.expired_timelocks(&**context.bitcoin_wallet).await?,
                context
                    .env_config
                    .bitcoin_blocks_till_confirmed_upper_bound_assumption,
            ) {
                return Ok(AliceState::WaitingForCancelTimelockExpiration {
                    monero_wallet_restore_blockheight,
                    transfer_proof: None,
                    state3,
                });
            }

            let outpost_proof = constructed.transfer_proof()?;
            let proof = crate::protocol_v5::core::monero::TransferProof::new(
                crate::protocol_v5::core::monero::TxHash(outpost_proof.tx_hash().0.clone()),
                crate::protocol_v5::core::monero::PrivateKey::from_slice(
                    outpost_proof.tx_key().as_bytes(),
                )
                .context("Invalid Monero transaction key")?,
            );

            match context.monero_wallet.publish_constructed(&constructed).await {
                Ok(_) => {}
                Err(error) => {
                    if !context
                        .monero_wallet
                        .transaction_present(&constructed.tx_hash)
                        .await
                        .unwrap_or(false)
                    {
                        return Err(error)
                            .context("Failed to publish the Monero lock transaction");
                    }
                }
            }
            tracing::info!(%swap_id, txid = %proof.tx_hash(), "ASB_XMR_LOCK_PROOF");
            Ok(AliceState::XmrLockTransactionSent {
                monero_wallet_restore_blockheight,
                transfer_proof: proof,
                state3,
            })
        }
        AliceState::XmrLockTransactionSent {
            monero_wallet_restore_blockheight,
            transfer_proof,
            state3,
        } => match state3.expired_timelocks(&**context.bitcoin_wallet).await? {
            ExpiredTimelocks::None { .. } => {
                // Match legacy behaviour: send the transfer proof as soon as
                // the Monero lock is visible in the pool (0 confirmations).
                // Waiting for a Monero block delays the proof by minutes and
                // needlessly races Bob's chain-based cancel deadline.
                let visible = until_cancel_timelock(
                    async {
                        context
                            .monero_wallet
                            .wait_for_tx_confirmations(
                                &crate::monero::TxHash(transfer_proof.tx_hash().0.clone()),
                                0,
                            )
                            .await
                            .context("Failed to wait for the Monero lock to become visible")
                    },
                    wait_for_cancel_timelock(&state3, &**context.bitcoin_wallet),
                )
                .await?;
                match visible {
                    Some(()) => Ok(AliceState::XmrLocked {
                        monero_wallet_restore_blockheight,
                        transfer_proof,
                        state3,
                    }),
                    // The cancel timelock expired while the lock was still not
                    // visible: Bob can cancel and refund, so follow that path.
                    None => Ok(AliceState::CancelTimelockExpired {
                        monero_wallet_restore_blockheight,
                        transfer_proof: Some(transfer_proof),
                        state3,
                    }),
                }
            }
            _ => Ok(AliceState::CancelTimelockExpired {
                monero_wallet_restore_blockheight,
                transfer_proof: Some(transfer_proof),
                state3,
            }),
        },
        AliceState::XmrLocked {
            monero_wallet_restore_blockheight,
            transfer_proof,
            state3,
        } => {
            // The transfer proof is still valid while the lock timelock has
            // not expired. A failed ack usually means Bob was offline or the
            // response was lost, not that the swap is dead. Keep retrying so
            // Bob can reconnect and redeem; only the timelock may end this.
            let mut cancel_timelock =
                Box::pin(wait_for_cancel_timelock(&state3, &**context.bitcoin_wallet));
            loop {
                let sent = tokio::select! {
                    result = handle.send_transfer_proof(transfer_proof.clone()) => result,
                    result = &mut cancel_timelock => {
                        result?;
                        return Ok(AliceState::CancelTimelockExpired {
                            monero_wallet_restore_blockheight,
                            transfer_proof: Some(transfer_proof),
                            state3: state3.clone(),
                        });
                    }
                };
                match sent {
                    Ok(()) => {
                        tracing::info!(%swap_id, "ASB_TRANSFER_PROOF_ACK");
                        return Ok(AliceState::XmrLockTransferProofSent {
                            monero_wallet_restore_blockheight,
                            transfer_proof,
                            state3: state3.clone(),
                        });
                    }
                    Err(error) => {
                        tracing::warn!(
                            %swap_id,
                            %error,
                            "transfer-proof acknowledgement failed; retrying until cancel timelock"
                        );
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
                            result = &mut cancel_timelock => {
                                result?;
                                return Ok(AliceState::CancelTimelockExpired {
                                    monero_wallet_restore_blockheight,
                                    transfer_proof: Some(transfer_proof),
                                    state3: state3.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
        AliceState::XmrLockTransferProofSent {
            monero_wallet_restore_blockheight,
            transfer_proof,
            state3,
        } => {
            let request = tokio::select! {
                request = handle.recv_encrypted_signature() => request?,
                result = wait_for_cancel_timelock(&state3, &**context.bitcoin_wallet) => {
                    result?;
                    return Ok(AliceState::CancelTimelockExpired {
                        monero_wallet_restore_blockheight,
                        transfer_proof: Some(transfer_proof),
                        state3,
                    });
                }
            };
            anyhow::ensure!(
                request.swap_id == swap_id,
                "received an encrypted signature for a different swap"
            );
            anyhow::ensure!(
                state3.verify_tx_redeem_encsig(&request.tx_redeem_encsig),
                "Bob's encrypted redeem signature is invalid"
            );
            Ok(AliceState::EncSigLearned {
                monero_wallet_restore_blockheight,
                transfer_proof,
                encrypted_signature: Box::new(request.tx_redeem_encsig),
                state3,
            })
        }
        AliceState::EncSigLearned {
            monero_wallet_restore_blockheight,
            transfer_proof,
            encrypted_signature,
            state3,
        } => {
            // Parity with the legacy driver (`protocol/alice/swap.rs`): only redeem
            // while the cancel timelock is open, retry the broadcast until it
            // expires, and never leave this state without a way into the cancel
            // path. The v5 arm used to broadcast once and propagate any error, so
            // after a restart past the cancel timelock it retried a redeem that
            // could never confirm and never reached the XMR refund (audit A-9).
            let redeem_started = std::time::Instant::now();
            let timelock = state3.expired_timelocks(&**context.bitcoin_wallet).await?;
            let timelock_open = matches!(timelock, ExpiredTimelocks::None { .. });
            tracing::info!(
                %swap_id,
                elapsed_ms = redeem_started.elapsed().as_millis() as u64,
                "ASB_REDEEM_TIMELOCK_CHECKED"
            );

            if timelock_open {
                let redeem = state3
                    .signed_redeem_transaction(*encrypted_signature)
                    .context("Failed to construct Bitcoin redeem transaction")?;
                let tx_lock_status = context.bitcoin_wallet.subscribe_to(state3.tx_lock.clone()).await;
                let attempt = retry_until_cancel(
                    swap_id,
                    || {
                        let transaction = redeem.clone();
                        let wallet = context.bitcoin_wallet.clone();
                        async move {
                            let (txid, subscription) =
                                wallet.broadcast_if_unseen(transaction, "redeem").await?;
                            tracing::info!(
                                %swap_id,
                                elapsed_ms = redeem_started.elapsed().as_millis() as u64,
                                "ASB_REDEEM_BROADCAST_RETURNED"
                            );
                            subscription.wait_until_seen().await?;
                            Ok(txid)
                        }
                    },
                    tx_lock_status.wait_until_confirmed_with(state3.cancel_timelock()),
                    REDEEM_RETRY_INITIAL_INTERVAL,
                    REDEEM_RETRY_MAX_INTERVAL,
                )
                .await?;

                if let RetryOutcome::Operation(redeem_txid) = attempt {
                    // Surface the broadcast immediately; the ASB_REDEEM_TX event only
                    // fires after finality, which leaves the UI silent for two blocks.
                    tracing::info!(
                        %swap_id,
                        %redeem_txid,
                        elapsed_ms = redeem_started.elapsed().as_millis() as u64,
                        "ASB_REDEEM_PUBLISHED"
                    );
                    return Ok(AliceState::BtcRedeemTransactionPublished {
                        transfer_proof,
                        state3,
                    });
                }
                // The cancel timelock expired while retrying: a broadcast can race
                // it, so look for the redeem before cancelling.
            }

            let redeem_status = context
                .bitcoin_wallet
                .status_of_script(&state3.tx_redeem())
                .await?;
            let redeem_known = redeem_transaction_is_known(redeem_status)?;
            match encsig_route(false, redeem_known) {
                EncSigRoute::RedeemAlreadyPublished => {
                    Ok(AliceState::BtcRedeemTransactionPublished {
                        transfer_proof,
                        state3,
                    })
                }
                _ => Ok(AliceState::CancelTimelockExpired {
                    monero_wallet_restore_blockheight,
                    transfer_proof: Some(transfer_proof),
                    state3,
                }),
            }
        }
        AliceState::BtcRedeemTransactionPublished { state3, .. } => {
            let subscription = context
                .bitcoin_wallet
                .subscribe_to(state3.tx_redeem())
                .await;
            tokio::time::timeout(
                context.env_config.bitcoin_lock_confirmed_timeout,
                subscription.wait_until_final(),
            )
            .await
            .context("Timed out waiting for Alice's Bitcoin redeem finality")?
            .context("Failed waiting for Alice's Bitcoin redeem finality")?;
            tracing::info!(%swap_id, "ASB_REDEEM_TX");
            // The v5 driver never emitted ASB_EARNED_BTC (the legacy driver does,
            // `protocol/alice/swap.rs`), so the GUI's "Earned" stayed at 0 BTC for
            // every v5 swap. Report what the redeem paid us, once, at finality.
            let earned = state3.tx_redeem().amount_sat();
            // The XMR this swap sold, so the GUI can keep an all-time "sold" total.
            let xmr_sold_piconero = state3.xmr.as_pico();
            tracing::info!(%swap_id, earned, xmr_sold_piconero, "ASB_EARNED_BTC");
            Ok(AliceState::BtcRedeemed)
        }
        AliceState::BtcRedeemed => Ok(AliceState::BtcRedeemed),
        AliceState::SafelyAborted => Ok(AliceState::SafelyAborted),
        AliceState::BtcEarlyRefundable { state3 } => {
            let Some(tx) = state3.signed_early_refund_transaction() else {
                return Ok(AliceState::SafelyAborted);
            };
            let tx = tx?;
            context
                .bitcoin_wallet
                .broadcast_if_unseen(tx, "early_refund")
                .await
                .context("Failed to publish Alice's early refund transaction")?;
            Ok(AliceState::BtcEarlyRefunded(state3))
        }
        AliceState::BtcEarlyRefunded(state3) => Ok(AliceState::BtcEarlyRefunded(state3)),
        AliceState::WaitingForCancelTimelockExpiration {
            monero_wallet_restore_blockheight,
            transfer_proof,
            state3,
        } => {
            loop {
                if !matches!(
                    state3.expired_timelocks(&**context.bitcoin_wallet).await?,
                    ExpiredTimelocks::None { .. }
                ) {
                    return Ok(AliceState::CancelTimelockExpired {
                        monero_wallet_restore_blockheight,
                        transfer_proof,
                        state3,
                    });
                }
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        }
        AliceState::CancelTimelockExpired {
            monero_wallet_restore_blockheight,
            transfer_proof,
            state3,
        } => {
            if state3
                .check_for_tx_cancel(&**context.bitcoin_wallet)
                .await?
                .is_none()
            {
                let txid = state3
                    .submit_tx_cancel(&**context.bitcoin_wallet)
                    .await
                    .context("Failed to publish Alice's Bitcoin cancel transaction")?;
                tracing::info!(%swap_id, %txid, "ASB_CANCEL_TX");
            }
            Ok(AliceState::BtcCancelled {
                monero_wallet_restore_blockheight,
                transfer_proof,
                state3,
            })
        }
        AliceState::BtcCancelled {
            monero_wallet_restore_blockheight,
            transfer_proof,
            state3,
        } => loop {
            if let Some(spend_key) = state3.refund_btc(&**context.bitcoin_wallet).await? {
                return Ok(AliceState::BtcRefunded {
                    monero_wallet_restore_blockheight,
                    transfer_proof,
                    spend_key,
                    state3,
                });
            }
            if matches!(
                state3.expired_timelocks(&**context.bitcoin_wallet).await?,
                ExpiredTimelocks::Punish
            ) {
                return Ok(AliceState::BtcPunishable {
                    monero_wallet_restore_blockheight,
                    transfer_proof,
                    state3,
                });
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        },
        AliceState::BtcRefunded {
            monero_wallet_restore_blockheight,
            transfer_proof,
            spend_key,
            state3,
        } => Ok(AliceState::XmrRefundable {
            monero_wallet_restore_blockheight,
            transfer_proof,
            spend_key,
            state3,
        }),
        AliceState::BtcPartiallyRefunded {
            monero_wallet_restore_blockheight,
            transfer_proof,
            spend_key,
            state3,
        } => Ok(AliceState::XmrRefundable {
            monero_wallet_restore_blockheight,
            transfer_proof,
            spend_key,
            state3,
        }),
        AliceState::XmrRefundable {
            transfer_proof,
            spend_key,
            state3,
            ..
        } => {
            // No proof means Alice never locked XMR (e.g. the cancel timelock
            // window was too tight to lock safely), so there is nothing to
            // refund. This used to be an error, so such a swap failed on every
            // start and never reached a terminal state (audit A-10). With the
            // retrying driver it would also have retried forever.
            let Some(transfer_proof) = transfer_proof else {
                tracing::info!(%swap_id, "ASB_SWAP_NOTHING_TO_REFUND");
                return Ok(AliceState::SafelyAborted);
            };
            let monero_daemon = context
                .monero_daemon
                .as_ref()
                .context("A Monero daemon is required to refund XMR")?;
            let pending = match context.store.load_pending_monero_refund(swap_id).await? {
                Some(pending) => pending,
                None => {
                    let private_spend = ::monero::PrivateKey::from_slice(&spend_key.as_bytes())
                        .context("Invalid Monero refund spend key")?;
                    let private_view = crate::monero::PrivateViewKey::from_private_key(
                        ::monero::PrivateKey::from_slice(&state3.v.0.as_bytes())
                            .context("Invalid Monero view key")?,
                    );
                    let destination = context.monero_wallet.get_main_address();
                    let sweep = monero_daemon
                        .construct_sweep_to_single(
                            &private_spend,
                            &private_view,
                            &crate::monero::TxHash(transfer_proof.tx_hash().0.clone()),
                            &destination,
                            context.monero_network,
                        )
                        .await
                        .context("Failed to construct Alice's Monero refund")?;
                    let pending = PendingMoneroLock {
                        tx_hash: sweep.tx_hash.0.clone(),
                        tx_blob: sweep.tx_hex.clone(),
                        tx_metadata: String::new(),
                        tx_key: String::new(),
                    };
                    context
                        .store
                        .store_pending_monero_refund(swap_id, &pending)
                        .await?;
                    tracing::info!(%swap_id, txid = %pending.tx_hash, "ASB_REFUND");
                    pending
                }
            };

            match monero_daemon.publish_raw_transaction(&pending.tx_blob).await {
                Ok(_) => {}
                Err(error) => {
                    if !context
                        .monero_wallet
                        .transaction_present(&crate::monero::TxHash(pending.tx_hash.clone()))
                        .await
                        .unwrap_or(false)
                    {
                        return Err(error).context("Failed to publish Alice's Monero refund");
                    }
                }
            }
            let refund_hash = crate::monero::TxHash(pending.tx_hash.clone());
            wait_rebroadcasting(
                || context.monero_wallet.wait_for_tx_confirmations(&refund_hash, 1),
                || async {
                    // Already-known errors are expected; a dropped transaction is
                    // put back in the pool.
                    match monero_daemon.publish_raw_transaction(&pending.tx_blob).await {
                        Ok(_) => tracing::info!(%swap_id, "Republished Alice's Monero refund while waiting for it to confirm"),
                        Err(error) => tracing::debug!(%swap_id, %error, "Monero refund republish was not needed or failed"),
                    }
                },
                REFUND_REBROADCAST_INTERVAL,
            )
            .await
            .context("Failed to wait for Alice's Monero refund confirmation")?;
            Ok(AliceState::XmrRefunded {
                state3: Some(state3),
            })
        }
        AliceState::BtcPunishable {
            monero_wallet_restore_blockheight,
            transfer_proof,
            state3,
        } => {
            if let Some(spend_key) = state3.refund_btc(&**context.bitcoin_wallet).await? {
                return Ok(AliceState::BtcRefunded {
                    monero_wallet_restore_blockheight,
                    transfer_proof,
                    spend_key,
                    state3,
                });
            }
            // `state3.punish_btc` waited for finality with no deadline and never
            // republished: a punish transaction the pool dropped would be waited
            // on forever (audit S-05, same pattern as A-12). Re-ensure the
            // broadcast every interval; `ensure_broadcasted` only sends it if the
            // network does not already have it.
            let signed_punish = state3
                .signed_punish_transaction()
                .context("Failed to complete Bitcoin punish transaction")?;
            wait_rebroadcasting(
                || {
                    let transaction = signed_punish.clone();
                    let wallet = context.bitcoin_wallet.clone();
                    async move {
                        let (txid, subscription) =
                            bitcoin_wallet::BitcoinWallet::ensure_broadcasted(
                                &**wallet,
                                transaction,
                                "punish",
                            )
                            .await?;
                        tracing::info!(%swap_id, %txid, "ASB_PUNISH_TX");
                        subscription.wait_until_final().await?;
                        Ok(())
                    }
                },
                || async {},
                PUNISH_REBROADCAST_INTERVAL,
            )
            .await
            .context("Failed to punish Bob")?;
            Ok(AliceState::BtcPunished {
                state3,
                transfer_proof,
            })
        }
        AliceState::BtcPunished { state3, transfer_proof } => {
            Ok(AliceState::BtcPunished { state3, transfer_proof })
        }
        AliceState::XmrRefunded { state3 } => Ok(AliceState::XmrRefunded { state3 }),
        other => Err(anyhow::Error::new(UnsupportedAliceState(other.to_string()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encsig_route_redeems_while_the_cancel_timelock_is_open() {
        assert_eq!(encsig_route(true, false), EncSigRoute::AttemptRedeem);
        // Whether the redeem is already known does not matter before expiry.
        assert_eq!(encsig_route(true, true), EncSigRoute::AttemptRedeem);
    }

    // Audit A-9: past the cancel timelock the state used to retry a redeem that
    // could never confirm and never reached the XMR refund.
    #[test]
    fn encsig_route_reaches_the_cancel_path_after_the_timelock_expired() {
        assert_eq!(encsig_route(false, false), EncSigRoute::CancelRefund);
    }

    #[test]
    fn encsig_route_waits_for_a_redeem_that_was_already_published() {
        assert_eq!(encsig_route(false, true), EncSigRoute::RedeemAlreadyPublished);
    }

    #[test]
    fn maker_retry_delay_backs_off_and_is_capped() {
        let secs: Vec<u64> = (1..=8).map(|n| maker_retry_delay(n).as_secs()).collect();
        assert_eq!(secs, vec![5, 10, 20, 40, 80, 120, 120, 120]);
        assert_eq!(maker_retry_delay(0).as_secs(), 5);
        assert_eq!(maker_retry_delay(u32::MAX).as_secs(), 120);
    }

    #[test]
    fn only_unsupported_states_and_a_stopped_event_loop_are_fatal() {
        let unsupported = anyhow::Error::new(UnsupportedAliceState("weird".to_string()));
        assert!(is_fatal_swap_error(&unsupported));

        let stopped = anyhow::anyhow!("protocol-5 maker event loop stopped")
            .context("Failed waiting for the encrypted signature");
        assert!(is_fatal_swap_error(&stopped));

        // Transient network errors must be retried, not fatal (audit A-1).
        let electrum = anyhow::anyhow!("Resource temporarily unavailable (os error 35)")
            .context("Failed to read the Bitcoin cancel timelock");
        assert!(!is_fatal_swap_error(&electrum));
        assert!(!is_fatal_swap_error(&anyhow::anyhow!("Failed to publish the Monero lock")));
    }

    #[test]
    fn retry_delay_for_retries_transient_errors_and_stops_on_fatal_ones() {
        let transient = anyhow::anyhow!("Electrum timed out");
        assert_eq!(retry_delay_for(&transient, 1), Some(Duration::from_secs(5)));
        assert_eq!(retry_delay_for(&transient, 3), Some(Duration::from_secs(20)));

        let fatal = anyhow::Error::new(UnsupportedAliceState("weird".to_string()));
        assert_eq!(retry_delay_for(&fatal, 1), None);
    }

    #[tokio::test]
    async fn a_maker_step_survives_transient_errors() {
        let calls = std::cell::Cell::new(0u32);
        let retries = std::cell::Cell::new(0u32);

        let value = retry_unless_fatal(
            || {
                calls.set(calls.get() + 1);
                let attempt = calls.get();
                async move {
                    if attempt < 4 {
                        Err(anyhow::anyhow!("Electrum timed out"))
                    } else {
                        Ok("next state")
                    }
                }
            },
            |_| Duration::ZERO,
            |_, _, _| retries.set(retries.get() + 1),
        )
        .await
        .unwrap();

        assert_eq!(value, "next state");
        assert_eq!(calls.get(), 4);
        assert_eq!(retries.get(), 3);
    }

    #[tokio::test]
    async fn a_fatal_error_ends_the_maker_step_without_retrying() {
        let calls = std::cell::Cell::new(0u32);

        let result: Result<()> = retry_unless_fatal(
            || {
                calls.set(calls.get() + 1);
                async { Err(anyhow::Error::new(UnsupportedAliceState("x".to_string()))) }
            },
            |_| Duration::ZERO,
            |_, _, _| {},
        )
        .await;

        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn alice_refuses_to_lock_past_timelock_margin() {
        let margin = 12;
        assert!(!timelock_window_is_safe(
            &ExpiredTimelocks::None { blocks_left: 12 },
            margin
        ));
        assert!(timelock_window_is_safe(
            &ExpiredTimelocks::None { blocks_left: 13 },
            margin
        ));
        for expired in [
            ExpiredTimelocks::Cancel { blocks_left: 1 },
            ExpiredTimelocks::WaitingForRemainingRefund { blocks_left: 1 },
            ExpiredTimelocks::RemainingRefund,
            ExpiredTimelocks::Punish,
        ] {
            assert!(!timelock_window_is_safe(&expired, margin));
        }
    }
}

#[cfg(test)]
mod monero_wait_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn the_timelock_ends_a_wait_that_never_finishes() {
        let outcome: Option<()> = until_cancel_timelock(
            std::future::pending::<Result<()>>(),
            async { Ok(()) },
        )
        .await
        .unwrap();
        assert_eq!(outcome, None);
    }

    #[tokio::test]
    async fn a_wait_that_finishes_first_wins() {
        let outcome = until_cancel_timelock(async { Ok(7u32) }, std::future::pending::<Result<()>>())
            .await
            .unwrap();
        assert_eq!(outcome, Some(7));
    }

    #[tokio::test]
    async fn a_failing_timelock_check_is_an_error_not_a_win() {
        let result: Result<Option<()>> = until_cancel_timelock(
            std::future::pending::<Result<()>>(),
            async { anyhow::bail!("electrum unreachable") },
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn a_waiting_refund_is_republished_until_it_confirms() {
        let attempts = Arc::new(AtomicU32::new(0));
        let rebroadcasts = Arc::new(AtomicU32::new(0));
        let (a, r) = (attempts.clone(), rebroadcasts.clone());
        wait_rebroadcasting(
            move || {
                let a = a.clone();
                async move {
                    // confirms on the third look; the first two never answer
                    if a.fetch_add(1, Ordering::SeqCst) < 2 {
                        std::future::pending::<()>().await;
                    }
                    Ok(())
                }
            },
            move || {
                let r = r.clone();
                async move {
                    r.fetch_add(1, Ordering::SeqCst);
                }
            },
            Duration::from_millis(20),
        )
        .await
        .unwrap();
        assert_eq!(rebroadcasts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_confirmation_error_is_not_swallowed() {
        let result = wait_rebroadcasting(
            || async { anyhow::bail!("double spent") },
            || async {},
            Duration::from_secs(5),
        )
        .await;
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod no_progress_tests {
    use super::*;

    #[test]
    fn an_identical_state_returned_at_once_is_paused() {
        assert_eq!(
            no_progress_delay("XmrRefunded", "XmrRefunded", Duration::from_millis(3)),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn a_state_that_changed_is_not_paused() {
        assert_eq!(no_progress_delay("XmrLocked", "XmrLockTransferProofSent", Duration::from_millis(3)), None);
    }

    #[test]
    fn an_identical_state_after_a_real_wait_is_not_paused() {
        assert_eq!(no_progress_delay("XmrLocked", "XmrLocked", Duration::from_secs(30)), None);
    }
}

