use anyhow::{bail, Context, Result};
use bdk::blockchain::Blockchain;
use jni::JNIEnv;
use libp2p::Multiaddr;
use std::time::Duration;
use swap::bitcoin::wallet::Subscription;
use swap::bitcoin::{ExpiredTimelocks, TxCancel, TxRefund};
use swap::bitcoin::{Transaction, Txid};
use swap::cli::EventLoopHandle;
use swap::monero;
use swap::monero::wallet::{wait_for_confs, WatchRequest};
use swap::monero::{Address, InsufficientFunds, TxHash};
use swap::network::quote::SwapDisconnected;
use swap::network::swap_setup::bob::NewSwap;
use swap::protocol::bob;
use swap::protocol::bob::swap::is_complete;
use swap::protocol::bob::BobState;
use swap::{bitcoin, util};
use tokio::select;
use uuid::Uuid;

const LOCK_BROADCAST_RETRY_INITIAL_INTERVAL: Duration = Duration::from_secs(30);
const LOCK_BROADCAST_RETRY_MAX_INTERVAL: Duration = Duration::from_secs(5 * 60);
const RESCUE_BTC_LOCK_INTERVAL: Duration = Duration::from_secs(15 * 60);

#[allow(clippy::too_many_arguments)]
pub async fn run(multiaddr: Multiaddr, swap: bob::Swap, env: &JNIEnv<'_>) -> Result<BobState> {
    run_until(multiaddr, swap, is_complete, &env).await
}

pub async fn run_until(
    multiaddr: Multiaddr,
    mut swap: bob::Swap,
    is_target_state: fn(&BobState) -> bool,
    env: &JNIEnv<'_>,
) -> Result<BobState> {
    let mut current_state = swap.state;
    let mut notified_of_conf = false;
    let mut notified_of_lock = false;
    util::on_swap_running(&env, swap.id.to_string(), multiaddr);

    // Ensure history can find this swap even if setup fails or the process exits
    // before the first successful state transition.
    swap.db
        .insert_latest_state(swap.id, current_state.clone().into())
        .await?;

    while !is_target_state(&current_state) && util::get_running_swap(&env) {
        current_state = next_state(
            swap.id,
            current_state.clone(),
            &mut swap.event_loop_handle,
            swap.bitcoin_wallet.as_ref(),
            swap.monero_wallet.as_ref(),
            swap.db.as_ref(),
            swap.monero_receive_address,
            &env,
            &mut notified_of_conf,
            &mut notified_of_lock,
        )
        .await?;

        swap.db
            .insert_latest_state(swap.id, current_state.clone().into())
            .await?;
    }

    if !util::get_running_swap(&env) {
        bail!(SwapDisconnected)
    }

    Ok(current_state)
}

/// Cancel and refund fees are committed at setup and only paid if the swap
/// fails. Refuse swaps where the 2x-buffered pair would consume more than 20%
/// of the locked BTC amount; the expected failure cost is about half of that.
///
/// The combined ceiling also keeps the buffered proposals compatible with an
/// ASB still running the old 4% fee cap: 20% split by the 150/140 vbyte
/// cancel/refund ratio is 10.34% and 9.66%, both below the 12% (3x4%) ceiling
/// that ASB's sanity check accepts.
const MAX_FAILURE_PATH_FEE_PERCENT: u64 = 20;

fn ensure_failure_path_fee_is_reasonable(
    btc_amount: bitcoin::Amount,
    tx_cancel_fee: bitcoin::Amount,
    tx_refund_fee: bitcoin::Amount,
) -> Result<()> {
    let amount_sats = btc_amount.to_sat().max(1);
    let failure_path_fee = tx_cancel_fee.to_sat() + tx_refund_fee.to_sat();
    let max_allowed = amount_sats * MAX_FAILURE_PATH_FEE_PERCENT / 100;

    if failure_path_fee > max_allowed {
        let percent = failure_path_fee as f64 / amount_sats as f64 * 100.0;
        bail!(
            "Swap amount is too small for current Bitcoin fees: cancel and refund \
             protection costs {} sats ({:.1}% of the swap). Retry when fees are \
             lower or use a larger swap.",
            failure_path_fee,
            percent
        );
    }

    Ok(())
}

async fn next_state(
    swap_id: Uuid,
    state: BobState,
    event_loop_handle: &mut EventLoopHandle,
    bitcoin_wallet: &bitcoin::Wallet,
    monero_wallet: &swap::monero::Wallet,
    db: &(dyn swap::protocol::Database + Send + Sync),
    monero_receive_address: monero::Address,
    env: &JNIEnv<'_>,
    notified_of_conf: &mut bool,
    notified_of_lock: &mut bool,
) -> Result<BobState> {
    tracing::debug!(%state, "Advancing state");

    Ok(match state {
        BobState::Started {
            btc_amount,
            change_address,
        } => {
            let tx_refund_fee = bitcoin_wallet
                .estimate_fee_vsize_with_rate_multiplier(
                    TxRefund::vsize(),
                    btc_amount,
                    bitcoin::CANCEL_REFUND_FEE_RATE_MULTIPLIER,
                )
                .await?;
            let tx_cancel_fee = bitcoin_wallet
                .estimate_fee_vsize_with_rate_multiplier(
                    TxCancel::vsize(),
                    btc_amount,
                    bitcoin::CANCEL_REFUND_FEE_RATE_MULTIPLIER,
                )
                .await?;

            ensure_failure_path_fee_is_reasonable(btc_amount, tx_cancel_fee, tx_refund_fee)?;

            let state2 = event_loop_handle
                .setup_swap(NewSwap {
                    swap_id,
                    btc: btc_amount,
                    tx_refund_fee,
                    tx_cancel_fee,
                    bitcoin_refund_address: change_address,
                })
                .await?;

            util::print_swap_log_ln(&env, "Registered swap with peer".to_string());

            BobState::SwapSetupCompleted(state2)
        }
        BobState::SwapSetupCompleted(state2) => {
            // Record the current monero wallet block height so we don't have to scan from
            // block 0 once we create the redeem wallet.
            // This has to be done **before** the Bitcoin is locked in order to ensure that
            // if Bob goes offline the recorded wallet-height is correct.
            // If we only record this later, it can happen that Bob publishes the Bitcoin
            // transaction, goes offline, while offline Alice publishes Monero.
            // If the Monero transaction gets confirmed before Bob comes online again then
            // Bob would record a wallet-height that is past the lock transaction height,
            // which can lead to the wallet not detect the transaction.
            // Sync the wallet first: get_height() returns the wallet's cached height, and
            // recording a stale value (a wallet that hasn't been refreshed recently) makes
            // the redeem-wallet scan start potentially years in the past.
            monero_wallet.refresh().await?;
            let monero_wallet_restore_blockheight = monero_wallet.block_height().await?;
            tracing::info!(
                restore_blockheight = ?monero_wallet_restore_blockheight,
                "Recording monero restore blockheight for the redeem wallet"
            );

            // Alice and Bob have exchanged info
            let (state3, tx_lock) = state2.lock_btc().await?;
            let signed_tx = bitcoin_wallet
                .sign_and_finalize(tx_lock.clone().into())
                .await
                .context("Failed to sign Bitcoin lock transaction")?;
            util::print_swap_log_ln(
                &env,
                format!(
                    "Signed Bitcoin lock transaction: {}",
                    signed_tx.txid().to_string()
                ),
            );

            BobState::BtcLockSigned {
                state3,
                signed_transaction: signed_tx,
                monero_wallet_restore_blockheight,
            }
        }
        BobState::BtcLockSigned {
            state3,
            signed_transaction,
            monero_wallet_restore_blockheight,
        } => {
            let mut backoff = LOCK_BROADCAST_RETRY_INITIAL_INTERVAL;
            let (txid, _sub) = loop {
                match bitcoin_wallet
                    .broadcast_if_unseen(signed_transaction.clone(), "lock")
                    .await
                {
                    Ok(result) => break result,
                    Err(error) => {
                        util::print_swap_log_ln(
                            &env,
                            format!(
                                "Bitcoin lock broadcast failed; retrying in {} seconds: {:#}",
                                backoff.as_secs(),
                                error
                            ),
                        );

                        if !util::get_running_swap(&env) {
                            bail!(SwapDisconnected);
                        }

                        select! {
                            _ = tokio::time::sleep(backoff) => {}
                            _ = crate::util::wait_for_swap_client_kill(&env) => {
                                bail!(SwapDisconnected);
                            }
                        }
                        backoff = (backoff * 2).min(LOCK_BROADCAST_RETRY_MAX_INTERVAL);
                    }
                }
            };
            util::print_swap_log_ln(
                &env,
                format!("Bitcoin lock transaction is known to Electrum: {}", txid),
            );

            BobState::BtcLocked {
                state3,
                monero_wallet_restore_blockheight,
            }
        }
        // Bob has locked Btc
        // Watch for Alice to Lock Xmr or for cancel timelock to elapse
        BobState::BtcLocked {
            state3,
            monero_wallet_restore_blockheight,
        } => {
            let tx_lock_status = bitcoin_wallet.subscribe_to(state3.tx_lock.clone()).await;

            if let ExpiredTimelocks::None = state3.current_epoch(bitcoin_wallet).await? {
                let lock_tx_confirms_needed: u32 = if *notified_of_conf {
                    720 // some high number outside of the swap, since the swap will be canceled/refunded or punished before 720... kinda a hacky solution
                } else {
                    1
                };
                let transfer_proof_watcher = event_loop_handle.recv_transfer_proof();
                let cancel_timelock_expires =
                    tx_lock_status.wait_until_confirmed_with(state3.cancel_timelock);
                let lock_transaction_confirms =
                    tx_lock_status.wait_until_confirmed_with(lock_tx_confirms_needed);

                // The 15-minute lock-rescue branch below returns to this
                // state, so notify the UI only once instead of repeating the
                // "Locked Bitcoin" message on every re-broadcast.
                if !*notified_of_lock {
                    tracing::info!("Waiting for Alice to lock Monero");
                    util::on_btc_locked(&env, state3.tx_lock.txid().clone().to_string());
                    *notified_of_lock = true;
                }

                select! {
                    transfer_proof = transfer_proof_watcher => {
                        let transfer_proof = transfer_proof?;

                        tracing::info!(txid = %transfer_proof.tx_hash(), "Alice locked Monero");

                        BobState::XmrLockProofReceived {
                            state: state3,
                            lock_transfer_proof: transfer_proof,
                            monero_wallet_restore_blockheight
                        }
                    }
                    result = cancel_timelock_expires => {
                        result?;
                        tracing::info!("Alice took too long to lock Monero, cancelling the swap");

                        let state4 = state3.cancel();
                        BobState::CancelTimelockExpired(state4)
                    }
                    result = lock_transaction_confirms => {
                        result?;
                        tracing::info!("Bitcoin lock transaction has confirmed");

                        util::on_btc_lock_tx_confirm(&env, state3.tx_lock.txid().clone().to_string());
                        *notified_of_conf = true;
                        BobState::BtcLocked { state3, monero_wallet_restore_blockheight }
                    }
                    _ = tokio::time::sleep(RESCUE_BTC_LOCK_INTERVAL) => {
                        match bitcoin_wallet
                            .sign_and_finalize(state3.tx_lock.clone().into())
                            .await
                        {
                            Ok(signed) => {
                                match bitcoin_wallet
                                    .broadcast_if_unseen(signed, "lock-rescue")
                                    .await
                                {
                                    Ok((txid, _)) => tracing::warn!(
                                        %txid,
                                        minutes = %RESCUE_BTC_LOCK_INTERVAL.as_secs() / 60,
                                        "Bitcoin lock transaction is still unconfirmed; re-broadcast through the current Electrum server"
                                    ),
                                    Err(error) => tracing::warn!(
                                        "Bitcoin lock re-broadcast failed, will retry: {:#}",
                                        error
                                    ),
                                }
                            }
                            Err(error) => tracing::warn!(
                                "Re-signing the Bitcoin lock transaction failed, will retry: {:#}",
                                error
                            ),
                        }

                        util::print_swap_log_ln(
                            &env,
                            "Still waiting for Bitcoin lock confirmation. The swap client is connected; no action is needed."
                                .to_string(),
                        );

                        BobState::BtcLocked { state3, monero_wallet_restore_blockheight }
                    }
                    _ = crate::util::wait_for_swap_client_kill(&env) => {
                        BobState::BtcLocked { state3, monero_wallet_restore_blockheight }
                    }
                }
            } else {
                let state4 = state3.cancel();
                BobState::CancelTimelockExpired(state4)
            }
        }
        BobState::XmrLockProofReceived {
            state,
            lock_transfer_proof,
            monero_wallet_restore_blockheight,
        } => {
            let tx_lock_status = bitcoin_wallet.subscribe_to(state.tx_lock.clone()).await;

            if let ExpiredTimelocks::None = state.current_epoch(bitcoin_wallet).await? {
                let watch_request = state.lock_xmr_watch_request(lock_transfer_proof.clone());
                util::on_xmr_lock_proof_received(
                    &env,
                    swap_id.to_string(),
                    (&lock_transfer_proof.tx_hash()).to_string(),
                );

                select! {
                    received_xmr = watch_for_transfer(&env, monero_wallet, watch_request) => {
                        match received_xmr {
                            Ok(monero::TxHash(_str)) => BobState::XmrLocked(state.xmr_locked(monero_wallet_restore_blockheight)),
                            Err(monero::InsufficientFunds { expected, actual }) => {
                                util::print_swap_log_ln(&env, format!("[WARNING] Insufficient Monero have been locked! {} vs {}, waiting to broadcast cancel transaction...", &expected.to_string(), &actual.to_string()));
                                tx_lock_status.wait_until_confirmed_with(state.cancel_timelock).await?;

                                BobState::CancelTimelockExpired(state.cancel())
                            },
                        }
                    }
                    result = tx_lock_status.wait_until_confirmed_with(state.cancel_timelock) => {
                        result?;
                        BobState::CancelTimelockExpired(state.cancel())
                    }
                    _ = crate::util::wait_for_swap_client_kill(&env) => {
                        BobState::XmrLockProofReceived { state, lock_transfer_proof, monero_wallet_restore_blockheight }
                    }
                }
            } else {
                BobState::CancelTimelockExpired(state.cancel())
            }
        }
        BobState::XmrLocked(state) => {
            let tx_lock_status = bitcoin_wallet.subscribe_to(state.tx_lock.clone()).await;

            if let Ok(state5) = state.check_for_tx_redeem(bitcoin_wallet).await {
                // this is in case we send the encrypted signature to alice, but we don't get confirmation that she received it. alice would be able to redeem the btc, but we would be stuck in xmrlocked, never being able to redeem the xmr.
                BobState::BtcRedeemed(state5)
            } else if let ExpiredTimelocks::None = state.expired_timelock(bitcoin_wallet).await? {
                // Alice has locked Xmr
                // Bob sends Alice his key

                select! {
                    result = event_loop_handle.send_encrypted_signature(state.tx_redeem_encsig()?) => {
                        match result {
                            Ok(_) => BobState::EncSigSent(state),
                            Err(error) => bail!("Failed to send encrypted signature to Alice: {:#}", error),
                        }
                    }
                    result = tx_lock_status.wait_until_confirmed_with(state.cancel_timelock) => {
                        result?;
                        BobState::CancelTimelockExpired(state.cancel())
                    }
                    _ = crate::util::wait_for_swap_client_kill(&env) => {
                        BobState::XmrLocked(state)
                    }
                }
            } else {
                BobState::CancelTimelockExpired(state.cancel())
            }
        }
        BobState::EncSigSent(state) => {
            let tx_lock_status = bitcoin_wallet.subscribe_to(state.tx_lock.clone()).await;

            if let ExpiredTimelocks::None = state.expired_timelock(bitcoin_wallet).await? {
                select! {
                    state5 = state.watch_for_redeem_btc(bitcoin_wallet) => {
                        BobState::BtcRedeemed(state5?)
                    }
                    result = tx_lock_status.wait_until_confirmed_with(state.cancel_timelock) => {
                        result?;
                        BobState::CancelTimelockExpired(state.cancel())
                    }
                    _ = crate::util::wait_for_swap_client_kill(&env) => {
                        BobState::EncSigSent(state)
                    }
                }
            } else {
                BobState::CancelTimelockExpired(state.cancel())
            }
        }
        BobState::BtcRedeemed(state) => {
            util::call_basic_listener_method(
                &env,
                swap_id.to_string(),
                util::ON_BTC_REDEEMED_METHOD,
            );
            let (spend_key, view_key) = state.xmr_keys();

            let wallet_file_name = swap_id.to_string();
            let tx_lock_id = state.tx_lock_id();
            if let Err(e) = monero_wallet
                .create_from_and_load(
                    wallet_file_name.clone(),
                    spend_key,
                    view_key,
                    state.monero_wallet_restore_blockheight,
                )
                .await
            {
                // In case we failed to refresh/sweep, when resuming the wallet might already
                // exist! This is a very unlikely scenario, but if we don't take care of it we
                // might not be able to ever transfer the Monero.
                util::print_swap_log_ln(
                    &env,
                    format!("Failed to generate XMR wallet from keys: {:#}", e),
                );
                util::print_swap_log_ln(
                    &env,
                    format!("Attempting fallback of opening the wallet if it already exists"),
                );
                monero_wallet.open(wallet_file_name).await?;
            }

            // Ensure that the generated wallet is synced so we have a proper balance
            util::call_basic_listener_method(
                &env,
                swap_id.to_string(),
                util::ON_START_REDEEM_XMR_SYNC_METHOD,
            );
            let refresh_result = select! {
                result = monero_wallet.refresh() => result,
                _ = crate::util::wait_for_swap_client_kill(&env) => {
                    return Ok(BobState::BtcRedeemed(state));
                }
            };
            refresh_result.context(
                "Failed to refresh the Monero redeem wallet before sweeping; the XMR remains unswept",
            )?;

            // A sweep txid persisted before a crash is authoritative: query it
            // directly instead of relying on `get_transfers`, which can be
            // unavailable while wallet-rpc restarts.
            let persisted_sweep = db
                .get_redeem_sweep_txid(swap_id)
                .await
                .context("Failed to read the persisted redeem sweep txid")?;

            if let Some(txid) = persisted_sweep {
                tracing::info!(
                    %txid,
                    %monero_receive_address,
                    "Found persisted XMR sweep txid; waiting for confirmation without re-sweeping"
                );
                monero_wallet
                    .wait_for_outgoing_sweep_txid(txid)
                    .await
                    .context("Failed to confirm the persisted Monero redeem sweep")?;
                return Ok(BobState::XmrRedeemed { tx_lock_id });
            }

            // Recover a sweep that was broadcast just before a crash instead
            // of broadcasting a second one, then wait for it to confirm.
            if !monero_wallet
                .has_outgoing_transfer_to(monero_receive_address)
                .await
                .context("Failed to inspect the redeem wallet's outgoing transfers")?
            {
                util::call_basic_listener_method(
                    &env,
                    swap_id.to_string(),
                    util::ON_START_XMR_SWEEP_METHOD,
                );
                let tx_hashes = monero_wallet
                    .sweep_all(monero_receive_address)
                    .await
                    .context("Failed to sweep the Monero redeem wallet")?;
                if tx_hashes.is_empty() {
                    bail!(
                        "Monero sweep returned no transaction; refusing to mark the swap as XmrRedeemed"
                    );
                }

                if let Some(tx_hash) = tx_hashes.first() {
                    if let Err(error) = db.insert_redeem_sweep_txid(swap_id, tx_hash.clone()).await {
                        tracing::warn!(
                            %tx_hash,
                            "Failed to persist the XMR sweep txid before waiting; resume may fall back to address matching: {:#}",
                            error
                        );
                    }
                }

                for tx_hash in tx_hashes {
                    tracing::info!(%monero_receive_address, txid=%tx_hash.0, "Successfully transferred XMR to wallet");
                }
            } else {
                tracing::info!(
                    %monero_receive_address,
                    "Found an already-published XMR sweep; waiting for confirmation instead of re-sweeping"
                );
            }

            monero_wallet
                .wait_for_outgoing_sweep_confirmation(monero_receive_address)
                .await
                .context("Failed to confirm the Monero redeem sweep")?;

            BobState::XmrRedeemed { tx_lock_id }
        }
        BobState::CancelTimelockExpired(state4) => {
            if state4.check_for_tx_cancel(bitcoin_wallet).await.is_err() {
                let (txid, _sub) = state4.submit_tx_cancel(bitcoin_wallet).await?;
                util::on_swap_canceled(&env, swap_id.to_string(), txid.to_string());
            }

            BobState::BtcCancelled(state4)
        }
        BobState::BtcCancelled(state) => {
            // Bob has cancelled the swap
            match state.expired_timelock(bitcoin_wallet).await? {
                ExpiredTimelocks::None => {
                    bail!(
                        "Internal error: canceled state reached before cancel timelock was expired"
                    );
                }
                ExpiredTimelocks::Cancel => {
                    let mut killed = false;
                    loop {
                        let status = state.cancel_transaction_status(bitcoin_wallet).await?;
                        if status.is_confirmed_with(1u32) {
                            break;
                        }
                        tracing::info!(%status, "waiting for cancel transaction confirmation before publishing refund");
                        select! {
                            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
                            _ = crate::util::wait_for_swap_client_kill(&env) => { killed = true; }
                        }
                        if killed {
                            break;
                        }
                    }
                    if killed {
                        BobState::BtcCancelled(state)
                    } else {
                        state.publish_refund_btc(bitcoin_wallet).await?;
                        BobState::BtcRefunded(state)
                    }
                }
                ExpiredTimelocks::Punish => {
                    tracing::info!("You have been punished for not refunding in time");
                    BobState::BtcPunished {
                        tx_lock_id: state.tx_lock_id(),
                    }
                }
            }
        }
        BobState::BtcRefunded(state4) => BobState::BtcRefunded(state4),
        BobState::BtcPunished { tx_lock_id } => BobState::BtcPunished { tx_lock_id },
        BobState::SafelyAborted => BobState::SafelyAborted,
        BobState::XmrRedeemed { tx_lock_id } => BobState::XmrRedeemed { tx_lock_id },
    })
}

pub async fn broadcast(
    bitcoin_wallet: &bitcoin::Wallet,
    transaction: Transaction,
    kind: &str,
) -> Result<(Txid, Subscription)> {
    bitcoin_wallet.broadcast(transaction, kind).await
}

pub async fn watch_for_transfer(
    env: &JNIEnv<'_>,
    monero_wallet: &swap::monero::Wallet,
    request: WatchRequest,
) -> Result<TxHash, InsufficientFunds> {
    let WatchRequest {
        conf_target,
        public_view_key,
        public_spend_key,
        transfer_proof,
        expected,
    } = request;

    let txid = transfer_proof.tx_hash();

    tracing::info!(
        %txid,
        target_confirmations = %conf_target,
        "Waiting for Monero transaction finality"
    );

    let address = Address::standard(
        monero_wallet.network,
        public_spend_key,
        public_view_key.into(),
    );

    let check_interval = tokio::time::interval(monero_wallet.sync_interval);

    wait_for_confs(
        Some(env),
        &monero_wallet.inner,
        transfer_proof,
        address,
        expected,
        conf_target,
        check_interval,
        monero_wallet.name.clone(),
    )
    .await?;

    Ok(txid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use swap::bitcoin::Amount;

    #[test]
    fn accepts_failure_fees_at_or_below_twenty_percent() {
        ensure_failure_path_fee_is_reasonable(
            Amount::from_sat(100_000),
            Amount::from_sat(300),
            Amount::from_sat(280),
        )
        .unwrap();

        // Exactly 20% is allowed.
        ensure_failure_path_fee_is_reasonable(
            Amount::from_sat(10_000),
            Amount::from_sat(1_000),
            Amount::from_sat(1_000),
        )
        .unwrap();
    }

    #[test]
    fn rejects_failure_fees_above_twenty_percent() {
        let error = ensure_failure_path_fee_is_reasonable(
            Amount::from_sat(10_000),
            Amount::from_sat(2_000),
            Amount::from_sat(2_000),
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(
            message.contains("too small"),
            "unexpected message: {message}"
        );
        assert!(message.contains("40.0%"), "unexpected message: {message}");
    }

    #[test]
    fn old_asb_compatibility_follows_from_the_combined_ceiling() {
        // A 20% combined budget split by the 150/140 vbyte ratio leaves both
        // buffered proposals below the 12% ceiling an old 4%-cap ASB accepts.
        let amount_sats = 100_000u64;
        let combined = amount_sats * MAX_FAILURE_PATH_FEE_PERCENT / 100;
        let cancel = combined * 150 / 290;
        let refund = combined * 140 / 290;

        assert!(cancel * 100 <= amount_sats * 12);
        assert!(refund * 100 <= amount_sats * 12);
    }
}
