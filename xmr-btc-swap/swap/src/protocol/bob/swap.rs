use crate::bitcoin::{ExpiredTimelocks, TxCancel, TxRefund};
use crate::cli::EventLoopHandle;
use crate::network::swap_setup::bob::NewSwap;
use crate::protocol::bob;
use crate::protocol::Database;
use crate::protocol::bob::state::*;
use crate::{bitcoin, monero};
use anyhow::{bail, Context, Result};
use std::time::Duration;
use tokio::select;
use uuid::Uuid;

/// Backoff bounds for retrying the lock broadcast after transient electrum
/// failures.
const LOCK_BROADCAST_RETRY_INITIAL_INTERVAL: Duration = Duration::from_secs(30);
const LOCK_BROADCAST_RETRY_MAX_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// How often to re-broadcast an unconfirmed lock transaction while waiting
/// for the seller to lock Monero (rescue for accepted-but-non-propagating
/// electrum servers).
const RESCUE_BTC_LOCK_INTERVAL: Duration = Duration::from_secs(15 * 60);

pub fn is_complete(state: &BobState) -> bool {
    matches!(
        state,
        BobState::BtcRefunded(..)
            | BobState::XmrRedeemed { .. }
            | BobState::BtcPunished { .. }
            | BobState::SafelyAborted
    )
}

/// Identifies states that have already processed the transfer proof.
/// This is used to be able to acknowledge the transfer proof multiple times (if it was already processed).
/// This is necessary because sometimes our acknowledgement might not reach Alice.
pub fn has_already_processed_transfer_proof(state: &BobState) -> bool {
    // This match statement MUST match all states which Bob can enter after receiving the transfer proof.
    // We do not match any of the cancel / refund states because in those, the swap cannot be successfull anymore.
    matches!(
        state,
        BobState::XmrLockProofReceived { .. }
            | BobState::XmrLocked(..)
            | BobState::EncSigSent(..)
            | BobState::BtcRedeemed(..)
            | BobState::XmrRedeemed { .. }
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn run(swap: bob::Swap) -> Result<BobState> {
    run_until(swap, is_complete).await
}

pub async fn run_until(
    mut swap: bob::Swap,
    is_target_state: fn(&BobState) -> bool,
) -> Result<BobState> {
    let mut current_state = swap.state;

    // Make the swap discoverable by history/resume before the first network or
    // on-chain action. This is harmless when resuming an already-persisted state.
    swap.db
        .insert_latest_state(swap.id, current_state.clone().into())
        .await?;

    while !is_target_state(&current_state) {
        current_state = next_state(
            swap.id,
            current_state.clone(),
            &mut swap.event_loop_handle,
            swap.bitcoin_wallet.as_ref(),
            swap.monero_wallet.as_ref(),
            swap.db.as_ref(),
            swap.monero_receive_address,
        )
        .await?;

        swap.db
            .insert_latest_state(swap.id, current_state.clone().into())
            .await?;
    }

    Ok(current_state)
}

async fn next_state(
    swap_id: Uuid,
    state: BobState,
    event_loop_handle: &mut EventLoopHandle,
    bitcoin_wallet: &bitcoin::Wallet,
    monero_wallet: &monero::Wallet,
    db: &(dyn Database + Send + Sync),
    monero_receive_address: monero::Address,
) -> Result<BobState> {
    tracing::debug!(%state, "Advancing state");

    Ok(match state {
        BobState::Started {
            btc_amount,
            change_address,
        } => {
            // PRINCIPAL-CRITICAL: if the cancel or refund is stuck on fees,
            // the refund never confirms inside alice's punish window and the
            // remote seller punishes — the locked BTC is LOST. These two get
            // the highest-confidence rate; they're only PAID when a swap
            // fails, so the over-provisioning is free insurance in the
            // happy path.
            let tx_refund_fee = bitcoin_wallet
                .estimate_fee_vsize_at(
                    TxRefund::vsize(),
                    btc_amount,
                    bitcoin::fee_targets::FEE_TARGET_PRINCIPAL_CRITICAL,
                )
                .await?;
            let tx_cancel_fee = bitcoin_wallet
                .estimate_fee_vsize_at(
                    TxCancel::vsize(),
                    btc_amount,
                    bitcoin::fee_targets::FEE_TARGET_PRINCIPAL_CRITICAL,
                )
                .await?;

            let state2 = event_loop_handle
                .setup_swap(NewSwap {
                    swap_id,
                    btc: btc_amount,
                    tx_refund_fee,
                    tx_cancel_fee,
                    bitcoin_refund_address: change_address,
                })
                .await?;

            tracing::info!(%swap_id, "Starting new swap");

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

            // Persist the exact signed transaction before attempting broadcast.
            // If the process exits after broadcast but before the next state write,
            // resume can safely inspect/rebroadcast this same transaction.
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
            // Retry the broadcast until Electrum accepts it: a transient
            // outage (or a dead server the pool has not yet rotated away
            // from) must not strand the swap - the transaction is signed,
            // deterministic and persisted, so every retry pushes the SAME
            // transaction.
            let mut backoff = LOCK_BROADCAST_RETRY_INITIAL_INTERVAL;
            loop {
                match bitcoin_wallet
                    .broadcast_if_unseen(signed_transaction.clone(), "lock")
                    .await
                {
                    Ok((txid, _)) => {
                        tracing::info!(%txid, "Bitcoin lock transaction is known to Electrum");
                        break BobState::BtcLocked {
                            state3,
                            monero_wallet_restore_blockheight,
                        };
                    }
                    Err(error) => {
                        tracing::warn!(
                            retry_in = %backoff.as_secs(),
                            "ASB_LOCK_BROADCAST_RETRY: Bitcoin lock broadcast failed, retrying: {:#}",
                            error
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(LOCK_BROADCAST_RETRY_MAX_INTERVAL);
                    }
                }
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
                let transfer_proof_watcher = event_loop_handle.recv_transfer_proof();
                tokio::pin!(transfer_proof_watcher);

                // Rescue watchdog: the lock transaction should confirm within
                // minutes. If the wait drags on, the electrum server that
                // accepted our broadcast may have dropped it without
                // propagating (65+ minute stall observed). Re-broadcast the
                // deterministic transaction through the current electrum
                // server (which the pool may have rotated): re-broadcasts of
                // a known transaction are deduplicated network-wide, so this
                // is always safe.
                let mut rescue_tick = tokio::time::interval(RESCUE_BTC_LOCK_INTERVAL);
                rescue_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut first_rescue_tick = true;

                tracing::info!("Waiting for Alice to lock Monero");

                let next_state = loop {
                    tokio::select! {
                        transfer_proof = &mut transfer_proof_watcher => {
                            let transfer_proof = transfer_proof?;

                            tracing::info!(txid = %transfer_proof.tx_hash(), "Alice locked Monero");

                            break BobState::XmrLockProofReceived {
                                state: state3,
                                lock_transfer_proof: transfer_proof,
                                monero_wallet_restore_blockheight
                            };
                        },
                        result = tx_lock_status
                            .wait_until_confirmed_with(state3.cancel_timelock) =>
                        {
                            result?;
                            tracing::info!("Alice took too long to lock Monero, cancelling the swap");

                            let state4 = state3.cancel();
                            break BobState::CancelTimelockExpired(state4);
                        },
                        _ = rescue_tick.tick() => {
                            // the interval fires immediately on the first tick
                            if first_rescue_tick {
                                first_rescue_tick = false;
                                continue;
                            }

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
                                            "ASB_LOCK_RESCUE: lock transaction still unconfirmed, re-broadcast through the current electrum server"
                                        ),
                                        Err(error) => tracing::warn!(
                                            "ASB_LOCK_RESCUE: re-broadcast failed, will retry: {:#}",
                                            error
                                        ),
                                    }
                                }
                                Err(error) => tracing::warn!(
                                    "ASB_LOCK_RESCUE: re-signing the lock transaction failed, will retry: {:#}",
                                    error
                                ),
                            }
                        },
                    }
                };
                next_state
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
                let watch_request = state.lock_xmr_watch_request(lock_transfer_proof);

                select! {
                    received_xmr = monero_wallet.watch_for_transfer(watch_request) => {
                        match received_xmr {
                            Ok(()) => BobState::XmrLocked(state.xmr_locked(monero_wallet_restore_blockheight)),
                            Err(monero::InsufficientFunds { expected, actual }) => {
                                tracing::warn!(%expected, %actual, "Insufficient Monero have been locked!");
                                tracing::info!(timelock = %state.cancel_timelock, "Waiting for cancel timelock to expire");

                                tx_lock_status.wait_until_confirmed_with(state.cancel_timelock).await?;

                                BobState::CancelTimelockExpired(state.cancel())
                            },
                        }
                    }
                    result = tx_lock_status.wait_until_confirmed_with(state.cancel_timelock) => {
                        result?;
                        BobState::CancelTimelockExpired(state.cancel())
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
                            Err(err) => {
                                tracing::error!(%err, "Failed to send encrypted signature to Alice");
                                bail!("Failed to send encrypted signature to Alice");
                            }
                        }
                    },
                    result = tx_lock_status.wait_until_confirmed_with(state.cancel_timelock) => {
                        result?;
                        BobState::CancelTimelockExpired(state.cancel())
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
                    },
                    result = tx_lock_status.wait_until_confirmed_with(state.cancel_timelock) => {
                        result?;
                        BobState::CancelTimelockExpired(state.cancel())
                    }
                }
            } else {
                BobState::CancelTimelockExpired(state.cancel())
            }
        }
        BobState::BtcRedeemed(state) => {
            let (spend_key, view_key) = state.xmr_keys();

            let wallet_file_name = swap_id.to_string();
            let sweep_started_at = std::time::Instant::now();
            tracing::info!(
                wallet = %wallet_file_name,
                restore_blockheight = ?state.monero_wallet_restore_blockheight,
                "BTC redeemed; beginning XMR lock sweep (wallet create/open, refresh, sweep_all). \
                 If the restore blockheight is far behind the chain tip this scan can take a while — \
                 it is not a hang."
            );
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
                tracing::warn!("Failed to generate monero wallet from keys: {:#}", e);
                tracing::info!(%wallet_file_name,
                    "Falling back to trying to open the the wallet if it already exists",
                );
                monero_wallet.open(wallet_file_name.clone()).await?;
            }

            // Ensure that the generated wallet is synced so we have a proper balance
            tracing::info!(
                wallet = %wallet_file_name,
                elapsed_secs = %sweep_started_at.elapsed().as_secs(),
                "Redeem wallet loaded; refreshing (this scans every block from the restore \
                 height and can take a long time on a fresh/wrong restore height)"
            );
            monero_wallet.refresh().await?;
            tracing::info!(
                wallet = %wallet_file_name,
                elapsed_secs = %sweep_started_at.elapsed().as_secs(),
                "Monero wallet refresh complete; sweeping lock funds to receive address"
            );
            // A sweep txid persisted before a crash is authoritative: query it
            // directly instead of relying on `get_transfers`, which can be
            // unavailable while wallet-rpc restarts.
            if let Some(txid) = db.get_redeem_sweep_txid(swap_id).await? {
                tracing::info!(
                    %txid,
                    wallet = %wallet_file_name,
                    "Found persisted XMR sweep txid; waiting for confirmation without re-sweeping"
                );
                monero_wallet.wait_for_outgoing_sweep_txid(txid).await?;
                return Ok(BobState::XmrRedeemed {
                    tx_lock_id: state.tx_lock_id(),
                });
            }

            // Recover a sweep that was broadcast just before a crash instead
            // of broadcasting a second one, then wait for it to confirm.
            if monero_wallet
                .has_outgoing_transfer_to(monero_receive_address)
                .await?
            {
                tracing::info!(
                    wallet = %wallet_file_name,
                    "Found an already-published XMR sweep; waiting for confirmation instead of re-sweeping"
                );
            } else {
                let tx_hashes = monero_wallet.sweep_all(monero_receive_address).await?;
                if tx_hashes.is_empty() {
                    anyhow::bail!(
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
            }

            monero_wallet
                .wait_for_outgoing_sweep_confirmation(monero_receive_address)
                .await?;

            tracing::info!(
                wallet = %wallet_file_name,
                elapsed_secs = %sweep_started_at.elapsed().as_secs(),
                "XMR lock sweep finished"
            );

            BobState::XmrRedeemed {
                tx_lock_id: state.tx_lock_id(),
            }
        }
        BobState::CancelTimelockExpired(state4) => {
            if state4.check_for_tx_cancel(bitcoin_wallet).await.is_err() {
                state4.submit_tx_cancel(bitcoin_wallet).await?;
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
                    loop {
                        let status = state.cancel_transaction_status(bitcoin_wallet).await?;
                        if status.is_confirmed_with(1u32) {
                            break;
                        }
                        tracing::info!(%status, "waiting for cancel transaction confirmation before publishing refund");
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    }
                    state.publish_refund_btc(bitcoin_wallet).await?;
                    BobState::BtcRefunded(state)
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
