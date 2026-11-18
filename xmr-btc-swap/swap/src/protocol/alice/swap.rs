//! Run an XMR/BTC swap in the role of Alice.
//! Alice holds XMR and wishes receive BTC.
use crate::asb::{EventLoopHandle, LatestRate};
use crate::bitcoin::ExpiredTimelocks;
use crate::env::Config;
use crate::monero::TransferProof;
use crate::protocol::alice::{AliceState, State3, Swap};
use crate::{bitcoin, monero};
use anyhow::{bail, Context, Result};
use backoff::backoff::Backoff;
use monero_rpc::wallet::BlockHeight;
use std::future::Future;
use std::time::Duration;
use tokio::select;
use tokio::time::timeout;
use uuid::Uuid;

pub(crate) const REDEEM_RETRY_INITIAL_INTERVAL: Duration = Duration::from_secs(1);
pub(crate) const REDEEM_RETRY_MAX_INTERVAL: Duration = Duration::from_secs(30);

/// Backoff bounds for retrying the XMR lock after the buyer's BTC is locked.
const XMR_LOCK_RETRY_INITIAL_INTERVAL: Duration = Duration::from_secs(30);
const XMR_LOCK_RETRY_MAX_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub async fn run<LR>(swap: Swap, rate_service: LR) -> Result<AliceState>
where
    LR: LatestRate + Clone,
{
    run_until(swap, |_| false, rate_service).await
}

#[tracing::instrument(name = "swap", skip(swap,exit_early,rate_service), fields(id = %swap.swap_id), err)]
pub async fn run_until<LR>(
    mut swap: Swap,
    exit_early: fn(&AliceState) -> bool,
    rate_service: LR,
) -> Result<AliceState>
where
    LR: LatestRate + Clone,
{
    let mut current_state = swap.state;

    while !is_complete(&current_state) && !exit_early(&current_state) {
        current_state = next_state(
            swap.swap_id,
            current_state,
            &mut swap.event_loop_handle,
            swap.bitcoin_wallet.as_ref(),
            swap.monero_wallet.as_ref(),
            &swap.env_config,
            rate_service.clone(),
        )
        .await?;

        swap.db
            .insert_latest_state(swap.swap_id, current_state.clone().into())
            .await?;
    }

    swap.monero_wallet.store().await?;

    Ok(current_state)
}

pub(crate) enum RetryOutcome<T> {
    Operation(T),
    Cancelled,
}

/// Retry transient Bitcoin redeem failures with exponential backoff until either the
/// operation succeeds or the cancel timelock expires. Each in-flight broadcast is allowed
/// to complete; cancellation is checked before attempts and during backoff.
pub(crate) async fn retry_until_cancel<T, F, Fut, C>(
    swap_id: Uuid,
    mut operation: F,
    cancel: C,
    initial_interval: Duration,
    max_interval: Duration,
) -> Result<RetryOutcome<T>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
    C: Future<Output = Result<()>>,
{
    let mut policy = backoff::ExponentialBackoff {
        initial_interval,
        current_interval: initial_interval,
        max_interval,
        // Keep retrying throughout the remaining cancel window, even when it is long.
        max_elapsed_time: None,
        ..backoff::ExponentialBackoff::default()
    };

    tokio::pin!(cancel);
    loop {
        // Check the safety boundary before starting each attempt. Do not interpret an
        // Electrum lookup failure as proof that the cancel timelock is still safe.
        tokio::select! {
            biased;
            result = &mut cancel => {
                result?;
                return Ok(RetryOutcome::Cancelled);
            }
            _ = std::future::ready(()) => {}
        }

        match operation().await {
            Ok(value) => return Ok(RetryOutcome::Operation(value)),
            Err(error) => {
                let Some(next_retry) = policy.next_backoff() else {
                    return Err(error.context("BTC redeem retry policy was exhausted"));
                };
                tracing::warn!(
                    %swap_id,
                    %error,
                    retry_in_ms = %next_retry.as_millis(),
                    "BTC redeem attempt failed; retrying before cancel timelock"
                );

                tokio::select! {
                    biased;
                    result = &mut cancel => {
                        result?;
                        return Ok(RetryOutcome::Cancelled);
                    }
                    _ = tokio::time::sleep(next_retry) => {}
                }
            }
        }
    }
}

pub(crate) fn redeem_transaction_is_known(status: bitcoin::wallet::ScriptStatus) -> Result<bool> {
    match status {
        bitcoin::wallet::ScriptStatus::InMempool | bitcoin::wallet::ScriptStatus::Confirmed(_) => {
            Ok(true)
        }
        bitcoin::wallet::ScriptStatus::Unseen => Ok(false),
        bitcoin::wallet::ScriptStatus::Retrying => {
            bail!("Electrum could not determine whether the BTC redeem transaction was published")
        }
    }
}

/// Best-effort fetch of the current Monero block height; `None` while the
/// wallet RPC is unreachable.
async fn fetch_restore_blockheight(monero_wallet: &monero::Wallet) -> Option<BlockHeight> {
    match monero_wallet.block_height().await {
        Ok(height) => Some(height),
        Err(error) => {
            tracing::warn!(
                "Failed to fetch Monero block height for restore height: {:#}",
                error
            );
            None
        }
    }
}

/// A missing restore height must not panic after the XMR lock was broadcast:
/// scanning from genesis is slow but safe, and keeps the swap recoverable.
fn restore_height_or_scan_from_start(
    swap_id: Uuid,
    restore_blockheight: Option<BlockHeight>,
) -> BlockHeight {
    restore_blockheight.unwrap_or_else(|| {
        tracing::warn!(
            %swap_id,
            "Monero restore height unavailable after XMR lock; recovery will scan from genesis"
        );
        BlockHeight { height: 0 }
    })
}

/// Lock the swap's XMR, retrying through transient Monero failures.
///
/// The buyer's Bitcoin is already locked when we reach this state: returning
/// an error here strands the swap (nothing re-drives it until a restart)
/// while the buyer waits on our timelocks. Failed attempts are reconciled
/// against the wallet's outgoing transfers first - an attempt that broadcast
/// before erroring (e.g. a timeout after submit), or a lock broadcast by a
/// previous process that died before persisting the proof, is adopted instead
/// of double-locked. A `transfer` error is treated as ambiguous: we never
/// issue a second transfer automatically, because Monero's wallet RPC has no
/// idempotency key. Subsequent iterations only reconcile; if no lock is found
/// before the cancel timelock expires, `None` is returned and the state
/// machine takes the usual abort/refund path.
async fn lock_xmr_with_retries(
    swap_id: Uuid,
    state3: &State3,
    monero_wallet: &monero::Wallet,
    bitcoin_wallet: &bitcoin::Wallet,
    restore_blockheight: &mut Option<BlockHeight>,
) -> Result<Option<TransferProof>> {
    let request = state3.lock_xmr_transfer_request();
    let mut backoff = XMR_LOCK_RETRY_INITIAL_INTERVAL;
    let mut transfer_attempted = false;

    loop {
        // Reconcile before every attempt: a previous attempt (this
        // process, or a resumed process that died between broadcast and
        // state persist) may have already broadcast the lock.
        let reconciliation_succeeded = match monero_wallet.find_outgoing_transfer(&request).await {
            Ok(Some(transfer_proof)) => {
                tracing::info!(
                    %swap_id,
                    tx = %transfer_proof.tx_hash(),
                    "ASB_XMR_LOCK_RECONCILED: lock transaction was already broadcast, adopting it"
                );
                return Ok(Some(transfer_proof));
            }
            Ok(None) => true,
            Err(error) => {
                tracing::warn!(%swap_id, "Lock reconciliation lookup failed: {:#}", error);
                false
            }
        };

        match state3.expired_timelocks(bitcoin_wallet).await {
            Ok(ExpiredTimelocks::None) => {}
            Ok(_) => {
                tracing::info!(%swap_id, "Cancel timelock expired while locking XMR");
                return Ok(None);
            }
            Err(error) => {
                // Transient electrum/daemon failures must not strand the
                // swap; the next iteration retries (the pools heal).
                tracing::warn!(
                    %swap_id,
                    "Timelock check failed while locking XMR: {:#}",
                    error
                );
            }
        }

        if restore_blockheight.is_none() {
            *restore_blockheight = fetch_restore_blockheight(monero_wallet).await;
        }

        // Never send while the reconciliation lookup itself is failing: an
        // earlier attempt could have broadcast and we would not know.
        if !reconciliation_succeeded {
            tracing::warn!(
                %swap_id,
                retry_in = %backoff.as_secs(),
                "ASB_XMR_LOCK_RECONCILIATION_RETRY: waiting before retrying the lookup"
            );
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(XMR_LOCK_RETRY_MAX_INTERVAL);
            continue;
        }

        // A transfer RPC error may mean the transaction was broadcast but the
        // response was lost. Do not try to send another one; only reconcile
        // until the transfer appears or the cancel timelock expires.
        if transfer_attempted {
            tracing::warn!(
                %swap_id,
                retry_in = %backoff.as_secs(),
                "ASB_XMR_LOCK_AMBIGUOUS: not re-sending after an errored transfer; waiting for reconciliation or timelock"
            );
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(XMR_LOCK_RETRY_MAX_INTERVAL);
            continue;
        }

        match monero_wallet.transfer(request.clone()).await {
            Ok(transfer_proof) => return Ok(Some(transfer_proof)),
            Err(error) => {
                transfer_attempted = true;
                tracing::warn!(
                    %swap_id,
                    retry_in = %backoff.as_secs(),
                    "ASB_XMR_LOCK_AMBIGUOUS: Monero lock transfer failed; reconciling instead of re-sending: {:#}",
                    error
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(XMR_LOCK_RETRY_MAX_INTERVAL);
            }
        }
    }
}

async fn next_state<LR>(
    swap_id: Uuid,
    state: AliceState,
    event_loop_handle: &mut EventLoopHandle,
    bitcoin_wallet: &bitcoin::Wallet,
    monero_wallet: &monero::Wallet,
    env_config: &Config,
    mut rate_service: LR,
) -> Result<AliceState>
where
    LR: LatestRate,
{
    let rate = rate_service
        .latest_rate()
        .map_or("NaN".to_string(), |rate| format!("{}", rate));

    tracing::info!(%state, %rate, "Advancing state");

    Ok(match state {
        AliceState::Started { state3 } => {
            tracing::info!(%swap_id, "ASB_SWAP_STARTED");
            let tx_lock_status = bitcoin_wallet.subscribe_to(state3.tx_lock.clone()).await;
            match timeout(
                env_config.bitcoin_lock_mempool_timeout,
                tx_lock_status.wait_until_seen(),
            )
            .await
            {
                Err(_) => {
                    tracing::info!(
                        minutes = %env_config.bitcoin_lock_mempool_timeout.as_secs_f64() / 60.0,
                        "TxLock lock was not seen in mempool in time",
                    );
                    tracing::info!(%swap_id, "ASB_BTC_LOCK_SEEN_ERR");
                    AliceState::SafelyAborted
                }
                Ok(res) => {
                    res?;
                    AliceState::BtcLockTransactionSeen { state3 }
                }
            }
        }
        AliceState::BtcLockTransactionSeen { state3 } => {
            tracing::info!(%swap_id, "ASB_BTC_LOCK_SEEN");
            let tx_lock_status = bitcoin_wallet.subscribe_to(state3.tx_lock.clone()).await;
            match timeout(
                env_config.bitcoin_lock_confirmed_timeout,
                tx_lock_status.wait_until_final(),
            )
            .await
            {
                Err(_) => {
                    tracing::info!(
                        confirmations_needed = %env_config.bitcoin_finality_confirmations,
                        minutes = %env_config.bitcoin_lock_confirmed_timeout.as_secs_f64() / 60.0,
                        "TxLock lock did not get enough confirmations in time",
                    );
                    tracing::info!(%swap_id, "ASB_BTC_LOCK_ERR");
                    AliceState::SafelyAborted
                }
                Ok(res) => {
                    res?;
                    AliceState::BtcLocked { state3 }
                }
            }
        }
        AliceState::BtcLocked { state3 } => {
            tracing::info!(%swap_id, "ASB_BTC_LOCK");
            match state3.expired_timelocks(bitcoin_wallet).await? {
                ExpiredTimelocks::None => {
                    // Record the current monero wallet block height so we don't have to scan from
                    // block 0 for scenarios where we create a refund wallet.
                    // Fetched best-effort inside the retry loop (a dead daemon
                    // must not strand the swap) with a final blocking fetch
                    // once the transfer succeeded.
                    let mut monero_wallet_restore_blockheight =
                        fetch_restore_blockheight(monero_wallet).await;

                    let transfer_proof = lock_xmr_with_retries(
                        swap_id,
                        &state3,
                        monero_wallet,
                        bitcoin_wallet,
                        &mut monero_wallet_restore_blockheight,
                    )
                    .await?;

                    match transfer_proof {
                        Some(transfer_proof) => {
                            if monero_wallet_restore_blockheight.is_none() {
                                monero_wallet_restore_blockheight =
                                    fetch_restore_blockheight(monero_wallet).await;
                            }
                            AliceState::XmrLockTransactionSent {
                                monero_wallet_restore_blockheight:
                                    restore_height_or_scan_from_start(
                                        swap_id,
                                        monero_wallet_restore_blockheight,
                                    ),
                                transfer_proof,
                                state3,
                            }
                        }
                        // Cancel timelock expired while we were retrying the
                        // lock: same outcome as the pre-check below.
                        None => AliceState::SafelyAborted,
                    }
                }
                _ => AliceState::SafelyAborted,
            }
        }
        AliceState::XmrLockTransactionSent {
            monero_wallet_restore_blockheight,
            transfer_proof,
            state3,
        } => match state3.expired_timelocks(bitcoin_wallet).await? {
            ExpiredTimelocks::None => {
                monero_wallet
                    .watch_for_transfer(state3.lock_xmr_watch_request(transfer_proof.clone(), 0))
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to watch for transfer of XMR in transaction {}",
                            transfer_proof.tx_hash()
                        )
                    })?;

                monero_wallet.store().await?;

                AliceState::XmrLocked {
                    monero_wallet_restore_blockheight,
                    transfer_proof,
                    state3,
                }
            }
            _ => AliceState::CancelTimelockExpired {
                monero_wallet_restore_blockheight,
                transfer_proof,
                state3,
            },
        },
        AliceState::XmrLocked {
            monero_wallet_restore_blockheight,
            transfer_proof,
            state3,
        } => {
            let tx_lock_status = bitcoin_wallet.subscribe_to(state3.tx_lock.clone()).await;

            tokio::select! {
                result = event_loop_handle.send_transfer_proof(transfer_proof.clone()) => {
                   result?;

                   AliceState::XmrLockTransferProofSent {
                       monero_wallet_restore_blockheight,
                       transfer_proof,
                       state3,
                   }
                },
                // TODO: We should already listen for the encrypted signature here.
                //
                // If we send Bob the transfer proof, but for whatever reason we do not receive an acknoledgement from him
                // we would be stuck in this state forever (deadlock). By listening for the encrypted signature here we
                // can still proceed to the next state even if Bob does not respond with an acknoledgement.
                result = tx_lock_status.wait_until_confirmed_with(state3.cancel_timelock) => {
                    result?;
                    AliceState::CancelTimelockExpired {
                        monero_wallet_restore_blockheight,
                        transfer_proof,
                        state3,
                    }
                }
            }
        }
        AliceState::XmrLockTransferProofSent {
            monero_wallet_restore_blockheight,
            transfer_proof,
            state3,
        } => {
            let txid = transfer_proof.tx_hash().to_string();
            tracing::info!(%swap_id, %txid, "ASB_XMR_LOCK_PROOF");
            let tx_lock_status = bitcoin_wallet.subscribe_to(state3.tx_lock.clone()).await;

            select! {
                biased; // make sure the cancel timelock expiry future is polled first
                result = tx_lock_status.wait_until_confirmed_with(state3.cancel_timelock) => {
                    result?;
                    AliceState::CancelTimelockExpired {
                        monero_wallet_restore_blockheight,
                        transfer_proof,
                        state3,
                    }
                }
                enc_sig = event_loop_handle.recv_encrypted_signature() => {
                    tracing::info!("Received encrypted signature");

                    AliceState::EncSigLearned {
                        monero_wallet_restore_blockheight,
                        transfer_proof,
                        encrypted_signature: Box::new(enc_sig?),
                        state3,
                    }
                }
            }
        }
        AliceState::EncSigLearned {
            monero_wallet_restore_blockheight,
            transfer_proof,
            encrypted_signature,
            state3,
        } => match state3.expired_timelocks(bitcoin_wallet).await? {
            ExpiredTimelocks::None => {
                let signed_transaction = state3
                    .signed_redeem_transaction(*encrypted_signature)
                    .context("Failed to construct Bitcoin redeem transaction")?;
                let tx_lock_status = bitcoin_wallet.subscribe_to(state3.tx_lock.clone()).await;
                let attempt = retry_until_cancel(
                    swap_id,
                    || {
                        let transaction = signed_transaction.clone();
                        async move {
                            let (txid, subscription) = bitcoin_wallet
                                .broadcast_if_unseen(transaction, "redeem")
                                .await?;
                            subscription.wait_until_seen().await?;
                            Ok(txid)
                        }
                    },
                    tx_lock_status.wait_until_confirmed_with(state3.cancel_timelock),
                    REDEEM_RETRY_INITIAL_INTERVAL,
                    REDEEM_RETRY_MAX_INTERVAL,
                )
                .await?;

                match attempt {
                    RetryOutcome::Operation(_) => {
                        AliceState::BtcRedeemTransactionPublished { state3 }
                    }
                    RetryOutcome::Cancelled => {
                        // A broadcast can race timelock expiry. Only enter the cancel path
                        // if Electrum positively reports the redeem transaction as unseen.
                        let redeem_status =
                            bitcoin_wallet.status_of_script(&state3.tx_redeem()?).await?;
                        if redeem_transaction_is_known(redeem_status)? {
                            AliceState::BtcRedeemTransactionPublished { state3 }
                        } else {
                            AliceState::CancelTimelockExpired {
                                monero_wallet_restore_blockheight,
                                transfer_proof,
                                state3,
                            }
                        }
                    }
                }
            }
            _ => {
                // A restart may resume here after the redeem was broadcast but before
                // the state transition was persisted. Check for that deterministic tx
                // before advancing to the cancel path.
                let redeem_status = bitcoin_wallet.status_of_script(&state3.tx_redeem()?).await?;
                if redeem_transaction_is_known(redeem_status)? {
                    AliceState::BtcRedeemTransactionPublished { state3 }
                } else {
                    AliceState::CancelTimelockExpired {
                        monero_wallet_restore_blockheight,
                        transfer_proof,
                        state3,
                    }
                }
            }
        },
        AliceState::BtcRedeemTransactionPublished { state3 } => {
            let redeem = state3.tx_redeem()?;
            let subscription = bitcoin_wallet.subscribe_to(redeem.clone()).await;

            match subscription.wait_until_final().await {
                Ok(_) => {
                    let earned = redeem.get_amount();
                    tracing::info!(%earned, "ASB_EARNED_BTC");

                    AliceState::BtcRedeemed
                }
                Err(e) => {
                    bail!("The Bitcoin redeem transaction was seen in mempool, but waiting for finality timed out with {}. Manual investigation might be needed to ensure that the transaction was included.", e)
                }
            }
        }
        AliceState::CancelTimelockExpired {
            monero_wallet_restore_blockheight,
            transfer_proof,
            state3,
        } => {
            println!("CancelTimelockExpired");
            if state3.check_for_tx_cancel(bitcoin_wallet).await.is_err() {
                // If Bob hasn't yet broadcasted the cancel transaction, Alice has to publish it
                // to be able to eventually punish. Since the punish timelock is
                // relative to the publication of the cancel transaction we have to ensure it
                // gets published once the cancel timelock expires.
                if let Err(e) = state3.submit_tx_cancel(bitcoin_wallet).await {
                    tracing::debug!(
                        "Assuming cancel transaction is already broadcasted because: {:#}",
                        e
                    )
                }
            }

            AliceState::BtcCancelled {
                monero_wallet_restore_blockheight,
                transfer_proof,
                state3,
            }
        }
        AliceState::BtcCancelled {
            monero_wallet_restore_blockheight,
            transfer_proof,
            state3,
        } => {
            println!("BtcCancelled");
            let tx_refund_status = bitcoin_wallet.subscribe_to(state3.tx_refund()?).await;
            let tx_cancel_status = bitcoin_wallet.subscribe_to(state3.tx_cancel()).await;

            let refund_tx = state3.fetch_tx_refund(&bitcoin_wallet).await;
            if refund_tx.is_ok() {
                // catch here in case wait_until_seen() doesn't
                let refund_tx = refund_tx?;
                let spend_key = state3.extract_monero_private_key(refund_tx)?;
                AliceState::BtcRefunded {
                    monero_wallet_restore_blockheight,
                    transfer_proof,
                    spend_key,
                    state3,
                }
            } else {
                select! {
                    seen_refund = tx_refund_status.wait_until_seen() => {
                        seen_refund.context("Failed to monitor refund transaction")?;

                        let published_refund_tx = bitcoin_wallet.get_raw_transaction(state3.tx_refund()?.txid()).await?;
                        let spend_key = state3.extract_monero_private_key(published_refund_tx)?;

                        AliceState::BtcRefunded {
                            monero_wallet_restore_blockheight,
                            transfer_proof,
                            spend_key,
                            state3,
                        }
                    }
                    result = tx_cancel_status.wait_until_confirmed_with(state3.punish_timelock) => {
                        result?;

                        AliceState::BtcPunishable {
                            monero_wallet_restore_blockheight,
                            transfer_proof,
                            state3,
                        }
                    }
                }
            }
        }
        AliceState::BtcRefunded {
            monero_wallet_restore_blockheight,
            transfer_proof,
            spend_key,
            state3,
        } => {
            // The refund sweep must actually succeed before the swap may complete.
            // A transient failure (e.g. the Monero daemon disconnecting) used to be
            // logged and swallowed, marking the swap XmrRefunded while the funds sat
            // in the swap's temporary wallet (incident 2a779092). Retry with backoff
            // until the daemon recovers; a restart also retries because BtcRefunded
            // is non-terminal, and the sweep itself is idempotent.
            let backoff = backoff::ExponentialBackoffBuilder::new()
                .with_initial_interval(Duration::from_secs(30))
                .with_max_interval(Duration::from_secs(5 * 60))
                .with_max_elapsed_time(None)
                .build();

            backoff::future::retry(backoff, || {
                let state3 = state3.clone();
                let transfer_proof = transfer_proof.clone();
                let file_name = swap_id.to_string();
                let spend_key = spend_key.clone();
                async move {
                    state3
                        .refund_xmr(
                            monero_wallet,
                            monero_wallet_restore_blockheight,
                            file_name,
                            spend_key,
                            transfer_proof,
                        )
                        .await
                        .map_err(|error| {
                            tracing::warn!(
                                %swap_id,
                                %error,
                                "XMR refund sweep failed; retrying until the Monero daemon recovers"
                            );
                            backoff::Error::transient(error)
                        })
                }
            })
            .await?;

            monero_wallet.store().await?;

            AliceState::XmrRefunded
        }
        AliceState::BtcPunishable {
            monero_wallet_restore_blockheight,
            transfer_proof,
            state3,
        } => {
            let punish = state3.punish_btc(bitcoin_wallet).await;

            match punish {
                Ok(_) => AliceState::BtcPunished,
                Err(error) => {
                    tracing::warn!("Failed to publish punish transaction: {:#}", error);

                    // Upon punish failure we assume that the refund tx was included but we
                    // missed seeing it. In case we fail to fetch the refund tx we fail
                    // with no state update because it is unclear what state we should transition
                    // to. It does not help to race punish and refund inclusion,
                    // because a punish tx failure is not recoverable (besides re-trying) if the
                    // refund tx was not included.

                    tracing::info!("Falling back to refund");

                    let published_refund_tx = bitcoin_wallet
                        .get_raw_transaction(state3.tx_refund()?.txid())
                        .await?;

                    let spend_key = state3.extract_monero_private_key(published_refund_tx)?;

                    AliceState::BtcRefunded {
                        monero_wallet_restore_blockheight,
                        transfer_proof,
                        spend_key,
                        state3,
                    }
                }
            }
        }
        AliceState::XmrRefunded => AliceState::XmrRefunded,
        AliceState::BtcRedeemed => AliceState::BtcRedeemed,
        AliceState::BtcPunished => AliceState::BtcPunished,
        AliceState::SafelyAborted => AliceState::SafelyAborted,
    })
}

pub fn is_complete(state: &AliceState) -> bool {
    matches!(
        state,
        AliceState::XmrRefunded
            | AliceState::BtcRedeemed
            | AliceState::BtcPunished
            | AliceState::SafelyAborted
    )
}

/// This function is used to check if Alice is in a state where it is clear that she has already received the encrypted signature from Bob.
/// This allows us to acknowledge the encrypted signature multiple times
/// If our acknowledgement does not reach Bob, he might send the encrypted signature again.
pub(crate) fn has_already_processed_enc_sig(state: &AliceState) -> bool {
    matches!(
        state,
        AliceState::EncSigLearned { .. }
            | AliceState::BtcRedeemTransactionPublished { .. }
            | AliceState::BtcRedeemed
    )
}

#[cfg(test)]
mod restore_height_tests {
    use super::restore_height_or_scan_from_start;
    use monero_rpc::wallet::BlockHeight;
    use uuid::Uuid;

    #[test]
    fn missing_restore_height_falls_back_to_genesis() {
        let height = restore_height_or_scan_from_start(Uuid::from_u128(0), None);

        assert_eq!(height.height, 0);
    }

    #[test]
    fn known_restore_height_is_preserved() {
        let height = restore_height_or_scan_from_start(
            Uuid::from_u128(0),
            Some(BlockHeight { height: 42 }),
        );

        assert_eq!(height.height, 42);
    }
}

#[cfg(test)]
mod redeem_retry_tests {
    use super::{retry_until_cancel, RetryOutcome};
    use anyhow::anyhow;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use uuid::Uuid;

    #[tokio::test]
    async fn transient_redeem_errors_retry_until_success() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let operation_attempts = attempts.clone();

        let result = retry_until_cancel(
            Uuid::from_u128(0),
            move || {
                let attempts = operation_attempts.clone();
                async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) < 2 {
                        Err(anyhow!("temporary Electrum error"))
                    } else {
                        Ok("redeem txid")
                    }
                }
            },
            std::future::pending::<anyhow::Result<()>>(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await
        .unwrap();

        assert!(matches!(result, RetryOutcome::Operation("redeem txid")));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn cancel_timelock_wins_if_ready_with_redeem_attempt() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let operation_attempts = attempts.clone();

        let result = retry_until_cancel(
            Uuid::from_u128(0),
            move || {
                let attempts = operation_attempts.clone();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, anyhow::Error>(())
                }
            },
            async { Ok(()) },
            Duration::ZERO,
            Duration::ZERO,
        )
        .await
        .unwrap();

        assert!(matches!(result, RetryOutcome::Cancelled));
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cancel_timelock_stops_retries_during_backoff() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let operation_attempts = attempts.clone();

        let result = retry_until_cancel(
            Uuid::from_u128(0),
            move || {
                let attempts = operation_attempts.clone();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Err::<(), _>(anyhow!("temporary Electrum error"))
                }
            },
            async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                Ok(())
            },
            Duration::from_millis(50),
            Duration::from_secs(30),
        )
        .await
        .unwrap();

        assert!(matches!(result, RetryOutcome::Cancelled));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn in_flight_redeem_attempt_finishes_before_retry_helper_returns() {
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let mut cancel_tx = Some(cancel_tx);

        let result = retry_until_cancel(
            Uuid::from_u128(0),
            move || {
                let cancel_tx = cancel_tx.take().unwrap();
                async move {
                    // Signal that the redeem attempt has started, then complete it. The
                    // retry helper awaits each operation without cancelling it.
                    let _ = cancel_tx.send(());
                    tokio::task::yield_now().await;
                    Ok::<_, anyhow::Error>("broadcast accepted")
                }
            },
            async move {
                let _ = cancel_rx.await;
                Ok(())
            },
            Duration::from_secs(1),
            Duration::from_secs(30),
        )
        .await
        .unwrap();

        assert!(matches!(
            result,
            RetryOutcome::Operation("broadcast accepted")
        ));
    }

    #[tokio::test]
    async fn in_flight_redeem_error_retries_after_backoff() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let operation_attempts = attempts.clone();

        let result = retry_until_cancel(
            Uuid::from_u128(0),
            move || {
                let attempts = operation_attempts.clone();
                async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err::<(), _>(anyhow!("temporary broadcast error"))
                    } else {
                        Ok(())
                    }
                }
            },
            std::future::pending::<anyhow::Result<()>>(),
            Duration::ZERO,
            Duration::ZERO,
        )
        .await
        .unwrap();

        assert!(matches!(result, RetryOutcome::Operation(())));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
