use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use jni::JNIEnv;
use monero::Address;
use rand::Rng;
use swap::database::SqliteDatabase;
use swap::env::Config;
use swap::protocol::Database as _;
use swap::protocol_v5::bob;
use swap::protocol_v5::core::monero::BlockHeight;
use swap::protocol_v5::db::bob::{Bob as BobV5, BobEndState};
use swap::protocol_v5::event_loop::{BobEventLoop, new_bob_swarm};
use swap::seed::Seed;
use swap::swap_error::{ErrorType, SwapError};
use swap::util;

use crate::model::resume_request::ResumeRequest;
use crate::wallet;
use crate::buy_xmr_v5::{redeem_failure_action, RedeemFailureAction};

pub(crate) async fn resume(
    env: JNIEnv<'_>,
    request: ResumeRequest,
    seed: Seed,
    env_config: Config,
    data_dir: PathBuf,
    swap_uuid: uuid::Uuid,
) {
    let swap_id = swap_uuid.to_string();
    let database = match SqliteDatabase::open_with_cipher(
        data_dir.join("sqlite"),
        Some(swap::database::StateCipher::from_seed(&seed)),
    )
    .await
    {
        Ok(value) => Arc::new(value),
        Err(error) => {
            util::on_swap_error(
                &env,
                SwapError {
                    swap_id,
                    error_type: ErrorType::FailedToLoadSwapDatabase,
                    error_message: format!("Failed to load swap database: {error}"),
                    fatal: true,
                },
            );
            return;
        }
    };

    let state: Option<BobV5> = match database.load_protocol_v5_state(swap_uuid, "bob").await {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Failed to load protocol-5 state: {error}"),
            );
            return;
        }
    };
    let Some(state) = state else {
        crate::util::report_setup_error(
            &env,
            &swap_id,
            "No protocol-5 state exists for this swap.".to_string(),
        );
        return;
    };

    if let BobV5::Done(end) = &state {
        match end {
            BobEndState::XmrRedeemed { .. } => util::on_swap_completed(&env, swap_id),
            BobEndState::SafelyAborted => util::call_basic_listener_method(
                &env,
                swap_id,
                util::ON_SWAP_SAFELY_ABORTED_METHOD,
            ),
            BobEndState::BtcRefunded(_)
            | BobEndState::BtcEarlyRefunded(_)
            | BobEndState::BtcReclaimConfirmed(_)
            | BobEndState::BtcMercyConfirmed(_) => util::call_basic_listener_method(
                &env,
                swap_id,
                util::ON_SWAP_REFUNDED_METHOD,
            ),
        }
        return;
    }

    if let BobV5::BtcPunished { .. } = &state {
        util::on_swap_error(
            &env,
            SwapError {
                swap_id,
                error_type: ErrorType::SwapPunished,
                error_message: "The swap was punished.".to_string(),
                fatal: true,
            },
        );
        return;
    }

    if matches!(
        state,
        BobV5::Started { .. } | BobV5::ExecutionSetupDone { .. }
    ) {
        util::on_swap_error(
            &env,
            SwapError {
                swap_id,
                error_type: ErrorType::UnknownError,
                error_message: "This protocol-5 swap never reached the Bitcoin lock; start a new swap instead of resuming setup.".to_string(),
                fatal: true,
            },
        );
        return;
    }

    let _ = std::fs::create_dir_all(data_dir.join("logs"));
    let _ = swap::cli::tracing::init(false, false, data_dir.join("logs"), Some(swap_uuid));

    util::print_swap_log_ln(&env, "Resuming Bitcoin wallet...".to_string());
    // Reopen the wallet that funded this swap: the per-swap wallet for new
    // swaps, or the old shared seed-keyed wallet for swaps started before the
    // switch. Never invent a new temporary wallet here — it would not contain
    // (or scan) the deposit address that is actually funded.
    let deposit_wallet_folder =
        wallet::resume_deposit_wallet_dir(&data_dir, &swap_uuid, &seed.fingerprint_hex());
    let electrum_url = match parse_electrum_url(&request.electrum_url) {
        Ok(url) => url,
        Err(message) => {
            crate::util::report_setup_error(&env, &swap_id, message);
            return;
        }
    };
    let bitcoin_init = wallet::init_bitcoin_wallet(
        electrum_url,
        &request.electrum_pool_urls,
        request.proxy.as_str(),
        &seed,
        deposit_wallet_folder,
        env_config,
        request.swaps_receive_index,
        request.swaps_change_index,
        request.dojo_url,
        request.dojo_api_key,
    );
    let bitcoin_wallet = match tokio::time::timeout(
        std::time::Duration::from_secs(420),
        bitcoin_init,
    )
    .await
    {
        Ok(Ok(value)) => Arc::new(value),
        Ok(Err(error)) => {
            util::on_swap_error(
                &env,
                SwapError {
                    swap_id,
                    error_type: ErrorType::FailedToInitBitcoinWallet,
                    error_message: format!("{error:#}"),
                    fatal: true,
                },
            );
            return;
        }
        Err(_) => {
            util::on_swap_error(
                &env,
                SwapError {
                    swap_id,
                    error_type: ErrorType::FailedToInitBitcoinWallet,
                    error_message: "Timed out after 420s opening/syncing the Bitcoin deposit wallet. Check the swap log for the last completed stage (wallet open, Dojo address cache, or Electrum sync).".to_string(),
                    fatal: true,
                },
            );
            return;
        }
    };
    util::print_swap_log_ln(
        &env,
        "Bitcoin wallet ready; loading Monero wallet...".to_string(),
    );

    let monero_daemon = match request.xmr_daemon_url.as_deref() {
        Some(url) => {
            // Any remote daemon goes through Tor; a loopback one (the forwarder) ignores the proxy.
            let proxy = Some(request.proxy.as_str());
            crate::util::connect_monero_daemon(url, proxy).await
        }
        None => None,
    };
    if let Some(daemon) = monero_daemon.as_ref() {
        if let Err(error) = crate::util::attach_witnesses(
            daemon,
            &request.xmr_witness_urls,
            request.xmr_witness_count,
            request.xmr_witness_required,
            &request.proxy,
            false,
        )
        .await
        {
            tracing::warn!(%error, "Could not set up the Monero witness nodes for the resumed swap");
        }
    }
    let check_wallet_height = match monero_daemon.as_ref() {
        Some(daemon) => daemon
            .chain_height()
            .await
            .ok()
            .map(|tip| swap::monero::redeem_restore_height(Some(tip), 0) as u32),
        None => None,
    };

    let monero_wallet = match wallet::init_monero_check_wallet(
        request.xmr_rpc_endpoint,
        env_config,
        format!("{swap_uuid}-monitoring-wallet").as_str(),
        check_wallet_height,
    )
    .await
    {
        Ok(value) => Arc::new(value),
        Err(error) => {
            util::on_swap_error(
                &env,
                SwapError {
                    swap_id,
                    error_type: ErrorType::FailedToInitMoneroWallet,
                    error_message: error.to_string(),
                    fatal: true,
                },
            );
            return;
        }
    };
    util::print_swap_log_ln(
        &env,
        "Monero wallet ready; resuming swap state...".to_string(),
    );

    let monero_receive_address: Address = match database.get_monero_address(swap_uuid).await {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Failed to load Monero receive address: {error}"),
            );
            return;
        }
    };

    let redeemed_state5 = match &state {
        BobV5::BtcRedeemed(state5) => Some(state5.clone()),
        BobV5::XmrRedeemConstructed { state: state5, .. }
        | BobV5::XmrRedeemPublished { state: state5, .. } => Some(state5.clone()),
        _ => None,
    };
    if let Some(state5) = redeemed_state5 {
        // This path returns before the regular `on_swap_resume_context` call
        // below, so the panel would otherwise stay blank (seller, amounts and
        // step list) for a redeem-only resume.
        if let Ok(seller_peer_id) = database.get_peer_id(swap_uuid).await {
            if let Ok(seller_addresses) = database.get_addresses(seller_peer_id).await {
                if let Some(seller_address) = seller_addresses.first().cloned() {
                    let (resume_btc_sats, resume_xmr_piconero) = resume_display_amounts(&state);
                    util::on_swap_resume_context(
                        &env,
                        swap_id.clone(),
                        seller_address,
                        monero_receive_address.clone(),
                        resume_btc_sats,
                        resume_xmr_piconero,
                    );
                }
            }
        }
        util::call_basic_listener_method(&env, swap_id.clone(), util::ON_BTC_REDEEMED_METHOD);
        match crate::util::redeem_xmr(
            &env,
            &swap_id,
            &*database,
            swap_uuid,
            &state5,
            &monero_wallet,
            monero_daemon.as_ref(),
            monero_receive_address,
            env_config,
        )
        .await
        {
            Ok(_) => util::on_swap_completed(&env, swap_id),
            Err(error) => crate::util::report_post_lock_error(
                &env,
                &swap_id,
                format!("Failed to redeem XMR: {error}"),
            ),
        }
        return;
    }

    let seller_peer_id = match database.get_peer_id(swap_uuid).await {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Failed to load seller peer id: {error}"),
            );
            return;
        }
    };
    let seller_addresses = match database.get_addresses(seller_peer_id).await {
        Ok(value) if !value.is_empty() => value,
        Ok(_) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                "No seller addresses recorded for this swap.".to_string(),
            );
            return;
        }
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Failed to load seller addresses: {error}"),
            );
            return;
        }
    };

    let mut swarm = match new_bob_swarm(
        seed.derive_libp2p_identity(),
        request.proxy_port,
        swap::protocol_v5::protocol_config(env_config),
        bitcoin_wallet.protocol_wallet(),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_post_lock_error(
                &env,
                &swap_id,
                format!("Failed to start protocol-5 network: {error}"),
            );
            return;
        }
    };
    let Some(seller_address) = seller_addresses.first().cloned() else {
        crate::util::report_setup_error(
            &env,
            &swap_id,
            "No seller address is known for this swap".to_string(),
        );
        return;
    };
    for address in seller_addresses {
        swarm.add_peer_address(seller_peer_id, address);
    }

    let (event_loop, mut handle) = BobEventLoop::new(swarm, seller_peer_id, database.clone(), seller_address.clone());
    let event_loop_task = tokio::spawn(event_loop.run());

    // Repopulate the swap panel on resume (legacy did this; v5 never did).
    let (resume_btc_sats, resume_xmr_piconero) = resume_display_amounts(&state);
    util::on_swap_resume_context(
        &env,
        swap_id.clone(),
        seller_address.clone(),
        monero_receive_address.clone(),
        resume_btc_sats,
        resume_xmr_piconero,
    );

    let cancel_already_published = matches!(&state, BobV5::BtcCancelPublished(_));
    let state4 = match state {
        BobV5::BtcLockReadyToPublish { state3, monero_wallet_restore_blockheight: persisted_height, .. } => {
            if let Err(error) = bob::publish_lock(&*database, swap_uuid, &bitcoin_wallet).await {
                crate::util::report_post_lock_error(
                    &env,
                    &swap_id,
                    format!("Failed to republish Bitcoin lock: {error}"),
                );
                event_loop_task.abort();
                return;
            }
            util::on_btc_locked(&env, state3.tx_lock.txid().to_string());
            resume_to_signature(
                &env,
                &swap_id,
                &database,
                &mut handle,
                swap_uuid,
                state3,
                persisted_height.height,
                &monero_wallet,
                monero_daemon.as_ref(),
                &bitcoin_wallet,
                env_config.bitcoin_finality_confirmations,
                env_config.monero_network,
                env_config.monero_finality_confirmations,
            )
            .await
        }
        BobV5::BtcLocked { state3, monero_wallet_restore_blockheight: persisted_height } => {
            // Report the restored state to the UI. Without this the panel stays
            // on "resuming" for the whole transfer-proof wait (legacy reported
            // state on resume; v5 did not).
            util::on_btc_locked(&env, state3.tx_lock.txid().to_string());
            resume_to_signature(
                &env,
                &swap_id,
                &database,
                &mut handle,
                swap_uuid,
                state3,
                persisted_height.height,
                &monero_wallet,
                monero_daemon.as_ref(),
                &bitcoin_wallet,
                env_config.bitcoin_finality_confirmations,
                env_config.monero_network,
                env_config.monero_finality_confirmations,
            )
            .await
        }
        BobV5::XmrLocked { state4 } | BobV5::EncSigReadyToBeSent { state4, .. } => {
            util::on_xmr_lock_proof_received(
                &env,
                swap_id.clone(),
                state4.lock_transfer_proof().tx_hash().0,
            );
            if let Err(error) = bob::wait_for_xmr_lock_finality(
                &state4,
                &monero_wallet,
                env_config.monero_finality_confirmations,
                Some(|txid: &str, confirmations: u64| {
                    util::on_xmr_lock_confirmation(&env, txid.to_string(), confirmations)
                }),
            )
            .await
            {
                crate::util::report_post_lock_error(
                    &env,
                    &swap_id,
                    format!("Failed to wait for Monero lock finality: {error}"),
                );
                event_loop_task.abort();
                return;
            }
            // The witness nodes must see the lock confirmed too, as on the main path (audit M-2).
            if let Err(error) = bob::witnesses_confirm_lock_for(
                monero_daemon.as_ref(),
                &state4,
                env_config.monero_finality_confirmations,
            )
            .await
            {
                crate::util::report_post_lock_error(
                    &env,
                    &swap_id,
                    format!("Witness Monero nodes have not confirmed the lock; not sending the signature yet: {error}"),
                );
                event_loop_task.abort();
                return;
            }
            if let Err(error) = bob::send_redeem_encrypted_signature(
                &*database,
                swap_uuid,
                state4.clone(),
                &handle,
            )
            .await
            {
                crate::util::report_post_lock_error(
                    &env,
                    &swap_id,
                    format!("Failed to resend redeem signature: {error}"),
                );
                event_loop_task.abort();
                return;
            }
            Some(state4)
        }
        BobV5::EncSigSent { state4, .. } => {
            util::on_xmr_lock_proof_received(
                &env,
                swap_id.clone(),
                state4.lock_transfer_proof().tx_hash().0,
            );
            Some(state4)
        }
        BobV5::XmrLockProofReceived { state, lock_transfer_proof, monero_wallet_restore_blockheight } => {
            let Some(tx_key) = lock_transfer_proof.tx_key else {
                crate::util::report_setup_error(
                    &env,
                    &swap_id,
                    "The persisted Monero lock proof has no transaction key; restart the swap to verify it.".to_string(),
                );
                event_loop_task.abort();
                return;
            };
            let lock_transfer_proof = swap::protocol_v5::core::monero::TransferProof {
                tx_hash: lock_transfer_proof.tx_hash,
                tx_key,
            };
            let verified = bob::verify_and_accept_xmr_lock_with_wallet(
                &*database,
                swap_uuid,
                state,
                lock_transfer_proof,
                monero_wallet_restore_blockheight,
                None,
                &monero_wallet,
                monero_daemon.as_ref(),
                env_config.monero_network,
            )
            .await;
            match verified {
                Ok(Some(state4)) => {
                    util::on_xmr_lock_proof_received(
                        &env,
                        swap_id.clone(),
                        state4.lock_transfer_proof().tx_hash().0,
                    );
                    if let Err(error) = bob::wait_for_xmr_lock_finality(
                        &state4,
                        &monero_wallet,
                        env_config.monero_finality_confirmations,
                        Some(|txid: &str, confirmations: u64| {
                            util::on_xmr_lock_confirmation(&env, txid.to_string(), confirmations)
                        }),
                    )
                    .await
                    {
                        crate::util::report_post_lock_error(
                            &env,
                            &swap_id,
                            format!("Failed to wait for Monero lock finality: {error}"),
                        );
                        event_loop_task.abort();
                        return;
                    }
                    // As on the main path: the witness nodes must see the lock confirmed (M-2).
                    if let Err(error) = bob::witnesses_confirm_lock_for(
                        monero_daemon.as_ref(),
                        &state4,
                        env_config.monero_finality_confirmations,
                    )
                    .await
                    {
                        crate::util::report_post_lock_error(
                            &env,
                            &swap_id,
                            format!("Witness Monero nodes have not confirmed the lock; not sending the signature yet: {error}"),
                        );
                        event_loop_task.abort();
                        return;
                    }
                    if let Err(error) = bob::send_redeem_encrypted_signature(
                        &*database,
                        swap_uuid,
                        state4.clone(),
                        &handle,
                    )
                    .await
                    {
                        crate::util::report_post_lock_error(
                            &env,
                            &swap_id,
                            format!("Failed to resend redeem signature: {error}"),
                        );
                        event_loop_task.abort();
                        return;
                    }
                    Some(state4)
                }
                Ok(None) => {
                    crate::util::report_setup_error(
                        &env,
                        &swap_id,
                        "Persisted Monero lock proof does not verify.".to_string(),
                    );
                    event_loop_task.abort();
                    return;
                }
                Err(error) => {
                    crate::util::report_post_lock_error(
                        &env,
                        &swap_id,
                        format!("Failed to verify persisted Monero lock proof: {error}"),
                    );
                    event_loop_task.abort();
                    return;
                }
            }
        }
        BobV5::BtcCancelPublished(state6)
        | BobV5::BtcCancelled(state6)
        | BobV5::CancelTimelockExpired(state6) => {
            if !cancel_already_published {
                crate::util::notify_protocol_v5_cancel_prepared(&env, &swap_id);
            }
            match bob::run_cancel_and_refund(
                &*database,
                swap_uuid,
                state6,
                &**bitcoin_wallet,
                |message| util::print_swap_log_ln(&env, message.to_string()),
                |txid| crate::util::notify_protocol_v5_cancel(&env, &swap_id, txid.to_string()),
                |txid| crate::util::notify_protocol_v5_refund(&env, &swap_id, txid.to_string()),
            )
            .await
            {
                Ok(outcome) => crate::util::report_refund_outcome(&env, &swap_id, &outcome),
                Err(error) => crate::util::report_cancel_refund_error(&env, &swap_id, "Failed to finish cancel/refund", &error),
            }
            event_loop_task.abort();
            return;
        }
        BobV5::BtcEarlyRefundPublished(state6) => {
            match bob::wait_for_early_refund_confirmation(
                &*database,
                swap_uuid,
                &state6,
                &**bitcoin_wallet,
            )
            .await
            {
                Ok(bob::EarlyRefundOutcome::CancelTimelockExpired) => crate::util::report_post_lock_error(&env, &swap_id, "Bitcoin early refund did not confirm before the cancel timelock; switching to cancel/refund.".to_string()),
                Ok(bob::EarlyRefundOutcome::Confirmed(txid)) => {
                    util::print_swap_log_ln(
                        &env,
                        format!("Bitcoin early refund transaction confirmed: {txid}"),
                    );
                    util::call_basic_listener_method(
                        &env,
                        swap_id,
                        util::ON_SWAP_REFUNDED_METHOD,
                    );
                }
                Err(error) => crate::util::report_post_lock_error(
                    &env,
                    &swap_id,
                    format!(
                        "Bitcoin early refund is published but not confirmed: {error}. Not reporting the swap as refunded."
                    ),
                ),
            }
            event_loop_task.abort();
            return;
        }
        // A full or partial refund that is published: wait for it, then (partial) go on to
        // the deposit. Reclaim and mercy states used to land here and re-send the refund.
        BobV5::BtcRefundPublished(state6) | BobV5::BtcPartialRefundPublished(state6) => {
            match bob::wait_for_refund_confirmation(
                &*database,
                swap_uuid,
                &state6,
                &**bitcoin_wallet,
                &|txid| crate::util::notify_protocol_v5_refund(&env, &swap_id, txid.to_string()),
                &|message| util::print_swap_log_ln(&env, message.to_string()),
            )
            .await
            {
                Ok(outcome) => crate::util::report_refund_outcome(&env, &swap_id, &outcome),
                Err(error) => crate::util::report_post_lock_error(
                    &env,
                    &swap_id,
                    format!(
                        "Bitcoin refund transaction is published but not confirmed: {error}. Not reporting the swap as refunded."
                    ),
                ),
            }
            event_loop_task.abort();
            return;
        }
        // The partial refund is confirmed: claim the seller's deposit back, or learn that the
        // seller burnt it (audit H-1). Used to stop with "Manual review required".
        BobV5::BtcPartiallyRefunded(state6)
        | BobV5::WaitingForReclaimTimelockExpiration(state6)
        | BobV5::ReclaimTimelockExpired(state6)
        | BobV5::BtcReclaimPublished(state6)
        | BobV5::BtcWithholdPublished(state6)
        | BobV5::BtcWithheld(state6)
        | BobV5::BtcMercyPublished(state6) => {
            match bob::reclaim_deposit(
                &*database,
                swap_uuid,
                &state6,
                &**bitcoin_wallet,
                &|message| util::print_swap_log_ln(&env, message.to_string()),
            )
            .await
            {
                Ok(outcome) => crate::util::report_refund_outcome(&env, &swap_id, &outcome),
                Err(error) => crate::util::report_post_lock_error(
                    &env,
                    &swap_id,
                    format!("Failed to claim back the Bitcoin deposit: {error}"),
                ),
            }
            event_loop_task.abort();
            return;
        }
        other => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Protocol-5 state {other:?} is not resumable yet."),
            );
            event_loop_task.abort();
            return;
        }
    };

    let Some(state4) = state4 else {
        event_loop_task.abort();
        return;
    };

    resume_after_lock(
        &env,
        &swap_id,
        &database,
        &state4,
        &bitcoin_wallet,
        &monero_wallet,
        monero_daemon.as_ref(),
        monero_receive_address,
        env_config,
    )
    .await;
    event_loop_task.abort();
}

/// The persisted height wins when it is set and not in the future; otherwise
/// the current height is the only usable value.
pub(crate) fn resume_restore_height(persisted: u64, current: u64) -> u64 {
    if persisted > 0 && persisted <= current {
        persisted
    } else {
        current
    }
}

/// How long the Monero daemon may take to say its chain height when a swap resumes. A small
/// request: the wallet is not scanned. If it does not answer in time, the height saved with the
/// swap is used.
const RESUME_TIP_READ_LIMIT: std::time::Duration = std::time::Duration::from_secs(20);

/// How long a legacy swap (no saved restore height) waits for the Monero wallet before the step
/// fails and the app resumes the swap again.
const RESUME_LEGACY_HEIGHT_BUDGET: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// The restore height for a resumed swap, from what is known without scanning the Monero wallet:
/// the height saved when the swap started (always before Alice's lock) and, when the daemon
/// answered, its current tip. `None` only for a legacy swap that saved no height while the daemon
/// does not answer: only then must the wallet be synced to read one.
///
/// A resumed swap used to sync the whole Monero wallet just to confirm the saved height, which
/// stalled for up to 16 minutes behind a weak node (swap 927e04b3, 2026-11-13) while nothing
/// looked at the Bitcoin cancel timelock, and returned the saved height anyway.
pub(crate) fn plan_resume_restore_height(persisted: u64, tip: Option<u64>) -> Option<u64> {
    match tip {
        Some(tip) => Some(resume_restore_height(persisted, tip)),
        None if persisted > 0 => Some(persisted),
        None => None,
    }
}

async fn read_daemon_tip(daemon: Option<&swap::monero::ng::Daemon>) -> Option<u64> {
    let daemon = daemon?;
    match tokio::time::timeout(RESUME_TIP_READ_LIMIT, daemon.chain_height()).await {
        Ok(Ok(tip)) => Some(tip),
        Ok(Err(error)) => {
            tracing::warn!(%error, "Could not read the Monero chain height on resume");
            None
        }
        Err(_) => {
            tracing::warn!(
                limit_seconds = RESUME_TIP_READ_LIMIT.as_secs(),
                "The Monero daemon did not answer the chain height request on resume"
            );
            None
        }
    }
}

#[cfg(test)]
mod restore_height_tests {
    use super::{plan_resume_restore_height, resume_restore_height};

    /// The step before the Bitcoin timelock is checked must not wait on a full Monero wallet sync
    /// (swap 927e04b3, 2026-11-13: 16 silent minutes, and the saved height came back anyway).
    #[test]
    fn resume_does_not_sync_the_whole_wallet_for_a_height_it_already_has() {
        let source = include_str!("resume_v5.rs");
        // anchored on a real line start: these literals would otherwise match this test itself
        let start = source.find("\nasync fn resume_to_signature(").unwrap();
        let end = start + source[start..].find("\nasync fn cancel_and_refund_after_lock(").unwrap();
        let body = &source[start..end];
        assert!(body.contains("plan_resume_restore_height(persisted_restore_height, tip)"));
        assert!(
            !body.contains("refresh_then_block_height()"),
            "the plain wallet sync would block the Bitcoin safety checks"
        );
    }

    #[test]
    fn the_saved_height_is_used_without_a_wallet_sync_when_the_daemon_is_silent() {
        assert_eq!(plan_resume_restore_height(2_249_933, None), Some(2_249_933));
    }

    #[test]
    fn the_saved_height_wins_when_the_daemon_tip_has_moved_on() {
        assert_eq!(plan_resume_restore_height(2_249_933, Some(2_250_100)), Some(2_249_933));
    }

    #[test]
    fn a_saved_height_ahead_of_the_daemon_tip_is_not_trusted() {
        assert_eq!(plan_resume_restore_height(3_100_000, Some(3_000_500)), Some(3_000_500));
    }

    #[test]
    fn a_legacy_swap_takes_the_daemon_tip_and_only_then_needs_the_wallet() {
        assert_eq!(plan_resume_restore_height(0, Some(3_000_500)), Some(3_000_500));
        assert_eq!(plan_resume_restore_height(0, None), None);
    }

    #[test]
    fn the_height_saved_at_swap_start_is_kept_when_the_tip_has_moved_on() {
        assert_eq!(resume_restore_height(3_000_000, 3_000_500), 3_000_000);
    }

    #[test]
    fn an_unset_height_falls_back_to_the_current_tip() {
        assert_eq!(resume_restore_height(0, 3_000_500), 3_000_500);
    }

    #[test]
    fn a_height_ahead_of_the_wallet_is_not_trusted() {
        assert_eq!(resume_restore_height(3_100_000, 3_000_500), 3_000_500);
    }
}

async fn resume_to_signature(
    env: &JNIEnv<'_>,
    swap_id: &str,
    database: &Arc<SqliteDatabase>,
    handle: &mut swap::protocol_v5::event_loop::BobEventLoopHandle,
    swap_uuid: uuid::Uuid,
    state3: swap::protocol_v5::machine::bob::State3,
    persisted_restore_height: u64,
    monero_wallet: &swap::monero::Wallet,
    monero_daemon: Option<&swap::monero::ng::Daemon>,
    bitcoin_wallet: &swap::bitcoin::Wallet,
    bitcoin_finality_confirmations: u32,
    network: monero::Network,
    monero_finality_confirmations: u64,
) -> Option<swap::protocol_v5::machine::bob::State4> {
    let tip = read_daemon_tip(monero_daemon).await;
    let restore_height = match plan_resume_restore_height(persisted_restore_height, tip) {
        Some(height) => {
            if tip.is_none() {
                util::print_swap_log_ln(
                    env,
                    format!("The Monero node did not answer in time: using the restore height saved with this swap ({height})."),
                );
            }
            BlockHeight { height }
        }
        None => {
            // A legacy swap with no saved height, and no daemon answer: the wallet must say.
            util::print_swap_log_ln(
                env,
                "This swap has no saved Monero restore height and the Monero node did not answer: syncing the Monero wallet to read it...".to_string(),
            );
            let read = monero_wallet
                .refresh_then_block_height_retrying(RESUME_LEGACY_HEIGHT_BUDGET, |attempt, elapsed, error| {
                    util::print_swap_log_ln(
                        env,
                        format!(
                            "Still waiting for the Monero node ({}s so far, attempt {attempt}): {}. Bitcoin stays locked.",
                            elapsed.as_secs(),
                            crate::util::humanize_error(&format!("{error:#}"))
                        ),
                    )
                })
                .await;
            match read {
                Ok(value) => BlockHeight { height: resume_restore_height(0, value.height as u64) },
                Err(error) => {
                    crate::util::report_post_lock_error(
                        env,
                        swap_id,
                        format!("Failed to refresh/read Monero height before reading restore height: {error:#}"),
                    );
                    return None;
                }
            }
        }
    };
    tracing::info!(
        restore_height = restore_height.height,
        persisted_restore_height,
        "Read v5 Monero restore blockheight"
    );

    let state3_for_cancel = state3.clone();
    let cancel_state = state3_for_cancel.clone().cancel(BlockHeight {
        height: restore_height.height as u64,
    });
    match cancel_state.check_for_tx_cancel(&**bitcoin_wallet).await {
        Ok(Some(_)) => {
            tracing::info!(swap_id, "V5_BOB_RESUME_CANCEL_SEEN");
            match bob::run_cancel_and_refund(
                &**database,
                swap_uuid,
                cancel_state,
                &**bitcoin_wallet,
                |message| util::print_swap_log_ln(env, message.to_string()),
                |txid| crate::util::notify_protocol_v5_cancel(env, swap_id, txid.to_string()),
                |txid| crate::util::notify_protocol_v5_refund(env, swap_id, txid.to_string()),
            )
            .await
            {
                Ok(outcome) => crate::util::report_refund_outcome(env, swap_id, &outcome),
                Err(error) => crate::util::report_cancel_refund_error(env, swap_id, "Failed to cancel/refund Bitcoin", &error),
            }
            return None;
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(swap_id, %error, "Failed to check for an existing Bitcoin cancel");
        }
    }

    let lock_txid = state3.tx_lock.txid().to_string();
    loop {
        // Same as the buy path: the redeem retry `continue`s back here, so a
        // stopped swap must end the loop (first review #6).
        if !util::get_running_swap(env) {
            return None;
        }
        match bob::run_until_signature_sent(
            &**database,
            handle,
            swap_uuid,
            state3_for_cancel.clone(),
            |state4| {
                util::on_xmr_lock_proof_received(
                    env,
                    swap_id.to_string(),
                    state4.lock_transfer_proof().tx_hash().0,
                )
            },
            |txid: &str, confirmations: u64| {
                util::on_xmr_lock_confirmation(env, txid.to_string(), confirmations)
            },
            || util::on_btc_lock_tx_confirm(env, lock_txid.clone()),
            || {
                util::print_swap_log_ln(
                    env,
                    format!("Alice's Bitcoin early refund was seen for swap {swap_id}; waiting for confirmation."),
                );
            },
            &**bitcoin_wallet,
            bitcoin_finality_confirmations,
            BlockHeight {
                height: restore_height.height as u64,
            },
            monero_wallet,
            monero_daemon,
            network,
            monero_finality_confirmations,
        )
        .await
        {
            Ok(Some(value)) => return Some(value),
            Ok(None) => {
                let state6 = state3_for_cancel.clone().cancel(BlockHeight {
                    height: restore_height.height as u64,
                });
                match bob::wait_for_early_refund_confirmation(&**database, swap_uuid, &state6, &**bitcoin_wallet).await {
                    Ok(bob::EarlyRefundOutcome::CancelTimelockExpired) => crate::util::report_post_lock_error(env, swap_id, "Bitcoin early refund did not confirm before the cancel timelock; switching to cancel/refund.".to_string()),
                    Ok(bob::EarlyRefundOutcome::Confirmed(txid)) => {
                        util::print_swap_log_ln(env, format!("Bitcoin early refund transaction confirmed: {txid}"));
                        util::call_basic_listener_method(env, swap_id.to_string(), util::ON_SWAP_REFUNDED_METHOD);
                    }
                    Err(error) => crate::util::report_post_lock_error(env, swap_id, format!("Bitcoin early refund is published but not confirmed: {error}. Resume from History to wait for confirmation.")),
                }
                return None;
            }
            Err(error) => {
                // Not fatal: Bitcoin is locked and the code below decides
                // between retry and cancel. Reporting it as a fatal error
                // stopped the client on every transient failure (swap
                // f7a162f7, 2026-11-06).
                let raw = format!("{error:#}");
                tracing::warn!(error = %raw, "Resume hit an error while Bitcoin is locked");
                util::print_swap_log_ln(
                    env,
                    format!(
                        "Resume hit an error: {}. Bitcoin stays locked while the timelock is checked.",
                        crate::util::humanize_error(&raw)
                    ),
                );

                let Some(timelock) = crate::util::retry_while_locked(
                    || state3_for_cancel.expired_timelock(&**bitcoin_wallet),
                    || util::get_running_swap(env),
                    |attempt, timelock_error, wait| {
                        let raw = format!("{timelock_error:#}");
                        tracing::warn!(attempt, error = %raw, "Cannot read the Bitcoin cancel timelock yet");
                        util::print_swap_log_ln(
                            env,
                            format!(
                                "Cannot read the Bitcoin cancel timelock yet: {}. Bitcoin stays locked; retrying in {}s (attempt {attempt}).",
                                crate::util::humanize_error(&raw),
                                wait.as_secs()
                            ),
                        )
                    },
                    crate::util::locked_swap_retry_delay,
                )
                .await
                else {
                    return None;
                };

                match redeem_failure_action(&timelock, handle.accepted_state4().is_some()) {
                    RedeemFailureAction::RetryRedeem => {
                        util::print_swap_log_ln(
                            env,
                            format!(
                                "Redeem not complete yet: {}. Bitcoin is still locked; retrying instead of canceling.",
                                crate::util::humanize_error(&format!("{error:#}"))
                            ),
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        continue;
                    }
                    RedeemFailureAction::ObserveOutcome => {
                        util::print_swap_log_ln(
                            env,
                            "Bitcoin cancel timelock expired after Alice's Monero lock was accepted; checking whether Alice redeemed before cancelling."
                                .to_string(),
                        );
                        return handle.accepted_state4();
                    }
                    RedeemFailureAction::CancelRefund => {}
                }

                util::print_swap_log_ln(
                    env,
                    "Bitcoin cancel timelock expired before the redeem completed; starting cancel/refund."
                        .to_string(),
                );
                cancel_and_refund_after_lock(
                    env,
                    swap_id,
                    database,
                    swap_uuid,
                    state3_for_cancel.clone(),
                    restore_height.height as u64,
                    bitcoin_wallet,
                )
                .await;
                return None;
            }
        }
    }
}

async fn cancel_and_refund_after_lock(
    env: &JNIEnv<'_>,
    swap_id: &str,
    database: &Arc<SqliteDatabase>,
    swap_uuid: uuid::Uuid,
    state3: swap::protocol_v5::machine::bob::State3,
    restore_height: u64,
    bitcoin_wallet: &swap::bitcoin::Wallet,
) {
    let state6 = match bob::cancel_lock(
        &**database,
        swap_uuid,
        state3,
        BlockHeight {
            height: restore_height,
        },
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_post_lock_error(
                env,
                swap_id,
                format!("Failed to persist the Bitcoin cancel path: {error}"),
            );
            return;
        }
    };
    crate::util::notify_protocol_v5_cancel_prepared(env, swap_id);
    match bob::run_cancel_and_refund(
        &**database,
        swap_uuid,
        state6,
        &**bitcoin_wallet,
        |message| util::print_swap_log_ln(env, message.to_string()),
        |txid| crate::util::notify_protocol_v5_cancel(env, swap_id, txid.to_string()),
        |txid| crate::util::notify_protocol_v5_refund(env, swap_id, txid.to_string()),
    )
    .await
    {
        Ok(outcome) => crate::util::report_refund_outcome(env, swap_id, &outcome),
        Err(error) => crate::util::report_cancel_refund_error(env, swap_id, "Failed to cancel/refund Bitcoin", &error),
    }
}

async fn resume_after_lock(
    env: &JNIEnv<'_>,
    swap_id: &str,
    database: &Arc<SqliteDatabase>,
    state4: &swap::protocol_v5::machine::bob::State4,
    bitcoin_wallet: &swap::bitcoin::Wallet,
    monero_wallet: &swap::monero::Wallet,
    monero_daemon: Option<&swap::monero::ng::Daemon>,
    monero_receive_address: Address,
    env_config: Config,
) {
    // A malformed id used to `unwrap()` here, and a panic in native code takes
    // the whole JVM down, possibly mid-swap.
    let swap_uuid = match parse_swap_uuid(swap_id) {
        Ok(uuid) => uuid,
        Err(message) => {
            crate::util::report_setup_error(env, swap_id, message);
            return;
        }
    };
    loop {
        if !util::get_running_swap(env) {
            return;
        }
        match bob::observe_btc_outcome(&**database, swap_uuid, state4, &**bitcoin_wallet).await {
            Ok(bob::BobOutcome::XmrRedeemed(state5)) => {
                util::call_basic_listener_method(env, swap_id.to_string(), util::ON_BTC_REDEEMED_METHOD);
                if let Err(error) = crate::util::redeem_xmr(
                    &env,
                    swap_id,
                    &**database,
                    swap_uuid,
                    &state5,
                    &monero_wallet,
                    monero_daemon,
                    monero_receive_address,
                    env_config,
                )
                .await
                {
                    crate::util::report_post_lock_error(
                        env,
                        swap_id,
                        format!("Failed to redeem XMR: {error}"),
                    );
                } else {
                    util::on_swap_completed(env, swap_id.to_string());
                }
                return;
            }
            Ok(bob::BobOutcome::Cancelled(state6)) => {
                crate::util::notify_protocol_v5_cancel_prepared(env, swap_id);
                match bob::run_cancel_and_refund(
                    &**database,
                    swap_uuid,
                    state6,
                    &**bitcoin_wallet,
                    |message| util::print_swap_log_ln(env, message.to_string()),
                    |txid| crate::util::notify_protocol_v5_cancel(env, swap_id, txid.to_string()),
                    |txid| crate::util::notify_protocol_v5_refund(env, swap_id, txid.to_string()),
                )
                .await
                {
                    Ok(outcome) => crate::util::report_refund_outcome(env, swap_id, &outcome),
                    Err(error) => crate::util::report_cancel_refund_error(env, swap_id, "Failed to cancel/refund Bitcoin", &error),
                }
                return;
            }
            Ok(bob::BobOutcome::Pending) => {}
            Err(error) => tracing::warn!("Protocol-5 resume poll failed: {error:#}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

/// BTC/XMR amounts for the swap panel on resume, mirroring the legacy helper.
fn resume_display_amounts(state: &BobV5) -> (u64, u64) {
    match state {
        BobV5::Started { btc_amount, .. } => (btc_amount.to_sat(), 0),
        BobV5::ExecutionSetupDone { state2 } => (
            state2.tx_lock.lock_amount().to_sat(),
            state2.xmr.as_pico(),
        ),
        BobV5::BtcLockReadyToPublish { state3, .. }
        | BobV5::BtcLocked { state3, .. }
        | BobV5::XmrLockProofReceived { state: state3, .. }
        | BobV5::XmrLockTransactionSeen { state: state3, .. }
        | BobV5::WaitingForCancelTimelockExpiration { state: state3, .. } => (
            state3.tx_lock.lock_amount().to_sat(),
            state3.xmr_amount().as_pico(),
        ),
        BobV5::XmrLocked { state4 }
        | BobV5::EncSigReadyToBeSent { state4, .. }
        | BobV5::EncSigSent { state4, .. } => (
            state4.tx_lock.lock_amount().to_sat(),
            state4.xmr_amount().as_pico(),
        ),
        BobV5::BtcRedeemed(state5)
        | BobV5::XmrRedeemConstructed { state: state5, .. }
        | BobV5::XmrRedeemPublished { state: state5, .. } => {
            (state5.btc_amount().to_sat(), state5.xmr_amount().as_pico())
        }
        BobV5::BtcPunished { state: state6, .. }
        | BobV5::CancelTimelockExpired(state6)
        | BobV5::BtcCancelPublished(state6)
        | BobV5::BtcCancelled(state6)
        | BobV5::BtcRefundPublished(state6)
        | BobV5::BtcEarlyRefundPublished(state6)
        | BobV5::BtcPartialRefundPublished(state6)
        | BobV5::BtcPartiallyRefunded(state6)
        | BobV5::WaitingForReclaimTimelockExpiration(state6)
        | BobV5::ReclaimTimelockExpired(state6)
        | BobV5::BtcReclaimPublished(state6)
        | BobV5::BtcWithholdPublished(state6)
        | BobV5::BtcWithheld(state6)
        | BobV5::BtcMercyPublished(state6) => (
            state6.tx_lock.lock_amount().to_sat(),
            state6.xmr.as_pico(),
        ),
        BobV5::Done(_) => (0, 0),
    }
}

/// Parses a swap id from the GUI without panicking.
fn parse_swap_uuid(swap_id: &str) -> Result<uuid::Uuid, String> {
    uuid::Uuid::parse_str(swap_id).map_err(|error| format!("Invalid swap id \"{swap_id}\": {error}"))
}

/// Parses the Electrum URL saved for a swap without panicking.
fn parse_electrum_url(url: &str) -> Result<url::Url, String> {
    url::Url::from_str(url).map_err(|error| format!("Invalid Electrum URL \"{url}\" for this swap: {error}"))
}

#[cfg(test)]
mod no_panic_parse_tests {
    use super::*;

    #[test]
    fn a_malformed_swap_id_is_an_error_not_a_panic() {
        assert!(parse_swap_uuid("not-a-uuid").is_err());
        assert!(parse_swap_uuid("").is_err());
        assert!(parse_swap_uuid("60889768-8a9d-4a40-ac37-e9cc1124ccdc").is_ok());
    }

    #[test]
    fn a_malformed_electrum_url_is_an_error_not_a_panic() {
        assert!(parse_electrum_url("::: not a url").is_err());
        assert!(parse_electrum_url("").is_err());
        assert!(parse_electrum_url("tcp://abcdefghijklmnop.onion:50001").is_ok());
    }
}

#[cfg(test)]
mod witness_check_tests {
    /// Audit M-2: every resume arm that sends the redeem signature first has the witness nodes
    /// confirm the Monero lock, as the main path does (source check: needs the JVM to run).
    #[test]
    fn every_resume_arm_checks_the_witnesses_before_the_signature() {
        let resume = include_str!("resume_v5.rs");
        let resume = &resume[..resume.find("mod witness_check_tests").unwrap()];
        let sends: Vec<usize> = resume
            .match_indices("bob::send_redeem_encrypted_signature(")
            .map(|(at, _)| at)
            .collect();
        assert_eq!(sends.len(), 2, "both signature-sending arms are covered");
        for send in sends {
            let before = &resume[..send];
            let finality = before.rfind("bob::wait_for_xmr_lock_finality(").unwrap();
            let witness = before.rfind("bob::witnesses_confirm_lock_for(");
            assert!(
                witness.is_some_and(|witness| witness > finality),
                "a resume arm sends the signature without the witness check (byte {})",
                send
            );
        }
    }
}

