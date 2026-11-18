use std::str::FromStr;
use std::sync::Arc;

use jni::objects::JClass;
use jni::sys::jstring;
use jni::JNIEnv;
use libp2p::Multiaddr;
use monero::Address;
use swap::bitcoin::TxLock;
use swap::env::{GetConfig, Mainnet, Testnet};
use swap::libp2p_ext::MultiAddrExt;
use swap::network::quote::{BidQuote as AppBidQuote, RefundPolicyWire as AppRefundPolicy, SwapDisconnected};
use swap::protocol_v5;
use swap::protocol_v5::core::bitcoin::ExpiredTimelocks;
use swap::protocol_v5::event_loop::{new_bob_swarm, BobEventLoop};
use swap::protocol_v5::{bob, machine};
use swap::swap_error::{ErrorType, SwapError};
use swap::{fs, util};

use url::Url;
use uuid::Uuid;

use crate::model::swap_request::SwapRequest;
use crate::wallet;

/// Connect to the seller before requesting the quote. A cold onion circuit can
/// take longer than the request's 10 s dial deadline; once the connection is
/// established the quote travels on it.
async fn connect_to_seller(
    swarm: &mut libp2p::Swarm<swap::protocol_v5::event_loop::TakerBehaviour>,
    seller: libp2p::PeerId,
) -> anyhow::Result<()> {
    use futures::StreamExt as _;
    use libp2p::swarm::dial_opts::DialOpts;
    use libp2p::swarm::SwarmEvent;
    use std::time::Duration;

    if swarm.is_connected(&seller) {
        return Ok(());
    }
    swarm.dial(DialOpts::peer_id(seller).build())?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        match tokio::time::timeout_at(deadline, swarm.select_next_some()).await {
            Err(_) => anyhow::bail!("timed out connecting to the seller after 45 s"),
            Ok(SwarmEvent::ConnectionEstablished { peer_id, .. }) if peer_id == seller => return Ok(()),
            Ok(SwarmEvent::OutgoingConnectionError {
                peer_id: Some(peer_id),
                error,
                ..
            }) if peer_id == seller => {
                anyhow::bail!("dial failed: {error}")
            }
            Ok(_) => {}
        }
    }
}

/// What to do when the redeem exchange returns an error.
///
/// Legacy never cancelled before the lock's cancel timelock expired. Keep that
/// invariant explicit so a transient proof/ack/signature error cannot move the
/// swap into cancel/refund early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedeemFailureAction {
    RetryRedeem,
    CancelRefund,
    /// Alice's lock was accepted, so the redeem signature may already be with
    /// her and she may have redeemed. Cancelling blindly would overwrite the
    /// swap with a cancel that can never confirm; follow the chain instead.
    ObserveOutcome,
}

pub(crate) fn redeem_failure_action(
    timelock: &ExpiredTimelocks,
    xmr_lock_accepted: bool,
) -> RedeemFailureAction {
    if !timelock.cancel_timelock_expired() {
        RedeemFailureAction::RetryRedeem
    } else if xmr_lock_accepted {
        RedeemFailureAction::ObserveOutcome
    } else {
        RedeemFailureAction::CancelRefund
    }
}

#[tokio::main]
#[no_mangle]
#[warn(unused_variables)]
pub async extern "system" fn Java_swap_client_ClientSwap_buyXmrV5(
    env: JNIEnv,
    _class: JClass,
    swap_request_json: jstring,
) {
    let swap_request_json_string = match util::get_string_value(&env, swap_request_json) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to read swap request");
            return;
        }
    };
    let swap_request: SwapRequest = match serde_json::from_str(&swap_request_json_string) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to parse swap request");
            return;
        }
    };

    let swap_id = swap_request.uuid.clone();
    let swap_uuid = match Uuid::from_str(swap_request.uuid.as_str()) {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(&env, &swap_id, format!("Invalid swap id: {error}"));
            return;
        }
    };
    let swap_id = swap_uuid.to_string();

    let bitcoin_electrum_rpc_url = match Url::from_str(swap_request.electrum_url.as_str()) {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Invalid Electrum URL: {error}"),
            );
            return;
        }
    };
    let monero_receive_address = match Address::from_str(swap_request.xmr_receive_address.as_str())
    {
        Ok(address) => address,
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Invalid Monero receive address: {error}"),
            );
            return;
        }
    };
    let seller = match Multiaddr::from_str(swap_request.libp2p_peer_address.as_str()) {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Invalid seller address: {error}"),
            );
            return;
        }
    };
    let bitcoin_refund_address =
        match swap::bitcoin::parse_checked_address(swap_request.refund_address.as_str()) {
            Ok(value) => value,
            Err(error) => {
                crate::util::report_setup_error(
                    &env,
                    &swap_id,
                    format!("Invalid Bitcoin refund address: {error}"),
                );
                return;
            }
        };

    let data_dir = match fs::system_data_dir() {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Failed to resolve the data directory: {error}"),
            );
            return;
        }
    };
    let env_config = if swap_request.testnet {
        Testnet::get_config()
    } else {
        Mainnet::get_config()
    };

    let seed = match crate::util::decode_seed(&swap_request.seed_base64) {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(&env, &swap_id, format!("Invalid seed: {error}"));
            return;
        }
    };

    let db_path = data_dir.join("sqlite");
    let database = match swap::database::SqliteDatabase::open_with_cipher(
        &db_path,
        Some(swap::database::StateCipher::from_seed(&seed)),
    )
    .await
    {
        Ok(value) => Arc::new(value),
        Err(error) => {
            util::on_swap_error(
                &env,
                SwapError {
                    swap_id: swap_id.clone(),
                    error_type: ErrorType::FailedToLoadSwapDatabase,
                    error_message: format!("Failed to load swap database: {error}"),
                    fatal: true,
                },
            );
            return;
        }
    };

    let _ = std::fs::create_dir_all(data_dir.join("logs"));
    let _ = swap::cli::tracing::init(false, false, data_dir.join("logs"), Some(swap_uuid));

    if monero_receive_address.network != env_config.monero_network {
        util::on_swap_error(
            &env,
            SwapError {
                swap_id: swap_id.clone(),
                error_type: ErrorType::UnknownError,
                error_message: format!(
                    "The provided Monero receive address is a {:?} address but the app is set to {:?}.",
                    monero_receive_address.network, env_config.monero_network
                ),
                fatal: true,
            },
        );
        return;
    }

    // One BDK database per seed keeps the UTXO/address cache warm across
    // swaps. The cache is seeded from both the paired wallet's indices and
    // this wallet's own next derivation index, so a lagging paired index
    // cannot hide a high-index SWAPS_DEPOSIT UTXO (§253).
    let deposit_wallet_folder = wallet::deposit_wallet_dir(&data_dir, &seed.fingerprint_hex());
    // The Bitcoin wallet setup (Electrum scan + Dojo over Tor, ~60 s) and the
    // Monero daemon + check wallet setup (~16 s) do not depend on each other, so
    // they run together instead of back to back: "Preparing swap" took ~83 s
    // (swap 139e7672, 2026-11-07), most of it waiting in sequence.
    let monero_proxy = swap_request.proxy.clone();
    let monero_daemon_url = swap_request.xmr_daemon_url.clone();
    let monero_rpc_endpoint = swap_request.xmr_rpc_endpoint.clone();
    let monitoring_wallet_name = format!("{swap_uuid}-monitoring-wallet");

    let bitcoin_init = wallet::init_bitcoin_wallet(
        bitcoin_electrum_rpc_url,
        &swap_request.electrum_pool_urls,
        swap_request.proxy.as_str(),
        &seed,
        deposit_wallet_folder,
        env_config,
        swap_request.swaps_receive_index,
        swap_request.swaps_change_index,
        swap_request.dojo_url,
        swap_request.dojo_api_key,
    );

    let witness_urls = swap_request.xmr_witness_urls.clone();
    let witness_required = swap_request.xmr_witness_required;
    let witness_count = swap_request.xmr_witness_count;
    let accepted_seller_fee_rate = swap_request.accepted_seller_fee_rate_sat_kvb;
    let monero_setup = async move {
        // Resolve the chain tip before creating the check wallet: its height is
        // the fallback the redeem height is recorded from, and a pinned constant
        // there is what turned redeems into 5,000-block rescans.
        let monero_daemon = match monero_daemon_url.as_deref() {
            Some(url) => {
                // Any remote daemon goes through Tor; a loopback one (the forwarder) ignores the proxy.
                let proxy = Some(monero_proxy.as_str());
                crate::util::connect_monero_daemon(url, proxy).await
            }
            None => None,
        };
        // A new swap checks its witnesses before any Bitcoin moves; too few that work refuses it.
        let witness_error = match monero_daemon.as_ref() {
            Some(daemon) => crate::util::attach_witnesses(
                daemon,
                &witness_urls,
                witness_count,
                witness_required,
                &monero_proxy,
                true,
            )
            .await
            .err(),
            None => None,
        };
        let check_wallet_height = match monero_daemon.as_ref() {
            Some(daemon) => daemon
                .chain_height()
                .await
                .ok()
                .map(|tip| swap::monero::redeem_restore_height(Some(tip), 0) as u32),
            None => None,
        };
        if check_wallet_height.is_none() {
            tracing::warn!(
                "No live Monero tip available; the check wallet falls back to the network constant"
            );
        }

        let monero_wallet = wallet::init_monero_check_wallet(
            monero_rpc_endpoint,
            env_config,
            monitoring_wallet_name.as_str(),
            check_wallet_height,
        )
        .await;
        (monero_daemon, monero_wallet, witness_error)
    };

    // Race the setup against a Disconnect: the Bitcoin scan alone can take minutes and has no stop
    // check of its own, so without this the app kept "Still preparing" long after the user left.
    let setup = tokio::select! {
        both = async { tokio::join!(bitcoin_init, monero_setup) } => Some(both),
        _ = util::wait_until_stopped(|| util::get_running_swap(&env), std::time::Duration::from_millis(250)) => None,
    };
    let Some((bitcoin_result, (monero_daemon, monero_result, witness_error))) = setup else {
        report_quote_stage_failure(&env, &swap_id, anyhow::anyhow!("The swap was stopped while it was being prepared"));
        return;
    };

    if let Some(error) = witness_error {
        util::on_swap_error(
            &env,
            SwapError {
                swap_id: swap_id.clone(),
                error_type: ErrorType::FailedToInitMoneroWallet,
                error_message: format!("{error:#}"),
                fatal: true,
            },
        );
        return;
    }

    let bitcoin_wallet = match bitcoin_result {
        Ok(value) => Arc::new(value),
        Err(error) => {
            util::on_swap_error(
                &env,
                SwapError {
                    swap_id: swap_id.clone(),
                    error_type: ErrorType::FailedToInitBitcoinWallet,
                    error_message: error.to_string(),
                    fatal: true,
                },
            );
            return;
        }
    };

    let monero_wallet = match monero_result {
        Ok(value) => Arc::new(value),
        Err(error) => {
            util::on_swap_error(
                &env,
                SwapError {
                    swap_id: swap_id.clone(),
                    error_type: ErrorType::FailedToInitMoneroWallet,
                    error_message: format!("{error:#}"),
                    fatal: true,
                },
            );
            return;
        }
    };

    use swap::protocol::Database as _;

    let seller_peer_id = match seller.extract_peer_id() {
        Some(value) => value,
        None => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                "Seller address is missing a peer ID.".to_string(),
            );
            return;
        }
    };

    if let Err(error) = database.insert_address(seller_peer_id, seller.clone()).await {
        crate::util::report_setup_error(
            &env,
            &swap_id,
            format!("Failed to save seller address: {error}"),
        );
        return;
    }
    if let Err(error) = database.insert_peer_id(swap_uuid, seller_peer_id).await {
        crate::util::report_setup_error(
            &env,
            &swap_id,
            format!("Failed to save seller peer id: {error}"),
        );
        return;
    }
    if let Err(error) = database
        .insert_monero_address(swap_uuid, monero_receive_address)
        .await
    {
        crate::util::report_setup_error(
            &env,
            &swap_id,
            format!("Failed to save Monero receive address: {error}"),
        );
        return;
    }

    let mut swarm = match new_bob_swarm(
        seed.derive_libp2p_identity(),
        swap_request.proxy_port,
        protocol_v5::protocol_config(env_config),
        bitcoin_wallet.protocol_wallet(),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Failed to start protocol-5 network: {error}"),
            );
            return;
        }
    };
    swarm.add_peer_address(seller_peer_id, seller.clone());

    if let Err(error) = connect_to_seller(&mut swarm, seller_peer_id).await {
        report_quote_stage_failure(&env, &swap_id, error);
        return;
    }

    let (event_loop, mut handle) = BobEventLoop::new(swarm, seller_peer_id, database.clone(), seller.clone());
    let _event_loop_task = tokio::spawn(event_loop.run());

    tracing::info!(swap_id = %swap_id, seller = %seller_peer_id, "V5_BOB_REQUESTING_QUOTE");
    let quote = match handle.request_quote().await {
        Ok(value) => value,
        Err(error) => {
            report_quote_stage_failure(&env, &swap_id, error);
            return;
        }
    };
    tracing::info!(swap_id = %swap_id, "V5_BOB_QUOTE_RECEIVED");
    if quote.max_quantity == swap::bitcoin::Amount::ZERO {
        util::on_swap_error(
            &env,
            SwapError {
                swap_id: swap_id.clone(),
                error_type: ErrorType::SellerInsufficientBalance,
                error_message: "Seller does not have sufficient Monero for swap.".to_string(),
                fatal: true,
            },
        );
        return;
    }

    // `determine_btc_to_swap` reads only price, min_quantity and max_quantity from this
    // adapter; the seller's real refund policy is shown by the seller list
    // (`list_sellers::refund_policy_label`). The fixed values below are placeholders,
    // not a statement about the seller (first review #8). Whether a PartialRefund seller
    // accepts a setup that carries no deposit is decided by the setup protocol, not here.
    let app_quote = AppBidQuote {
        price: quote.price,
        min_quantity: quote.min_quantity,
        max_quantity: quote.max_quantity,
        refund_policy: AppRefundPolicy::FullRefund,
        reserve_proof: None,
    };

    // Match the legacy buyer: max_giveable already reserves the local lock
    // policy fee, and the amount is min(max_giveable, seller_max). Do not
    // reserve an extra 3x "Alice fee" headroom here: that lowered the lock
    // amount by 308 sats, then passed the full headroom to the wallet as the
    // actual fee, burning the leftover when it was below dust.
    let deposit_sync_state = DepositSyncState::new();
    // On an accepted retry the seller's rate is known: reserve the lock fee it forces on us, or a
    // whole-balance swap comes up short when setup raises the lock fee to it (S-01, 2026-11-11).
    let min_lock_fee = accepted_seller_fee_rate
        .and_then(|rate| swap::bitcoin::wallet::v5_lock_fee_for_seller_rate(rate).ok())
        .unwrap_or(swap::bitcoin::Amount::ZERO);
    let max_givable = || bitcoin_wallet.max_giveable_v5_with_lock_fee_at_least(min_lock_fee);
    let estimate_fee = |amount| {
        bitcoin_wallet.estimate_fee_vsize(
            swap::protocol_v5::core::bitcoin::TxLock::weight().to_vbytes_ceil() as usize,
            amount,
        )
    };

    let (amount, fees) = match util::determine_btc_to_swap(
        swap_uuid,
        async { Ok(app_quote) },
        bitcoin_wallet.new_address(),
        || bitcoin_wallet.balance(),
        max_givable,
        || deposit_wait_sync(&bitcoin_wallet, &deposit_sync_state),
        estimate_fee,
        &env,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            report_quote_stage_failure(&env, &swap_id, error);
            return;
        }
    };

    util::on_swap_amount_determined(&env, amount);
    util::on_swap_running(&env, swap_id.clone(), seller.clone());
    tracing::info!(swap_id = %swap_id, %amount, "V5_BOB_AMOUNT_DETERMINED");

    // The first quote can be as old as the deposit wait. Setup checks the seller's XMR
    // amount against the quoted price (audit C-1), so ask again now: an honest seller
    // whose price moved while the user deposited is not refused, and a seller that quotes
    // one price and offers less in setup still is. Upstream keeps its quotes fresh with a
    // cached quote feed; this is the one-shot equivalent.
    let fresh_quote = match handle.request_quote().await {
        Ok(value) => value,
        Err(error) => {
            report_quote_stage_failure(&env, &swap_id, error);
            return;
        }
    };
    tracing::info!(
        swap_id = %swap_id,
        first_price = %quote.price,
        fresh_price = %fresh_quote.price,
        "V5_BOB_QUOTE_REFRESHED"
    );
    // The buyer commits automatically after setup, and setup only compares the seller's offer
    // with this fresh quote. A seller that showed a fair price to win the deposit and now quotes a
    // much higher one would get the whole deposit for a fraction of the Monero, so a price that
    // got worse by more than a small margin since the swap started is refused here, before
    // anything is signed. The swap stops (no automatic restart) and the user decides again.
    if let Err(reason) = check_fresh_price(quote.price.to_sat(), fresh_quote.price.to_sat()) {
        tracing::warn!(
            swap_id = %swap_id,
            first_price = %quote.price,
            fresh_price = %fresh_quote.price,
            "V5_BOB_PRICE_WORSENED"
        );
        crate::util::report_setup_error(&env, &swap_id, reason);
        return;
    }

    let estimate = match bitcoin_wallet.swap_fee_rate().await {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Failed to estimate swap fee rate: {error}"),
            );
            return;
        }
    };
    tracing::info!(swap_id = %swap_id, "V5_BOB_FEE_ESTIMATED");
    let fee_rate = match swap::protocol_v5::core::bitcoin::SwapFeeRate::from_estimate(estimate) {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Failed to determine swap fee rate: {error}"),
            );
            return;
        }
    };
    let fee_limits = match machine::common::swap_fee_limits(fee_rate, amount) {
        Ok(value) => crate::seller_fee::with_accepted_seller_rate(value, accepted_seller_fee_rate),
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Failed to compute fee limits: {error}"),
            );
            return;
        }
    };
    // Use the exact fee that max_giveable subtracted, exactly as the legacy
    // buyer did. This is amount + tx_lock_fee = spendable balance, so the
    // wallet does not create a dust change output.
    let tx_lock_fee = fees;
    tracing::info!(
        estimated_vsize = TxLock::vsize(),
        tx_lock_fee_sats = tx_lock_fee.to_sat(),
        "V5_BOB_LOCK_FEE_SELECTED"
    );

    let state2 = match handle
        .start_swap(
            swap::protocol_v5::p2p::protocols::swap_setup::bob::NewSwap {
                swap_id: swap_uuid,
                btc: amount,
                tx_lock_fee,
                fee_limits,
                bitcoin_refund_address,
                // checked against the maker's XMR amount in setup (audit C-1)
                quoted_price: fresh_quote.price,
            },
        )
        .await
    {
        Ok(value) => value,
        Err(error) => {
            let message = format!("Protocol-5 setup failed: {error}");
            // A seller asking more than 3x our fee estimate: let the user decide (S-01)
            if let Some(refusal) = crate::seller_fee::rate_above_limit(&message) {
                crate::seller_fee::on_seller_fee_rate_too_high(&env, &swap_id, refusal, fee_rate.0);
            }
            crate::util::report_setup_error(&env, &swap_id, message);
            return;
        }
    };

    let commit_started = std::time::Instant::now();
    let commit_result = handle.commit().await;
    tracing::info!(
        elapsed_ms = commit_started.elapsed().as_millis() as u64,
        "V5_BOB_COMMIT_ACKED"
    );
    if let Err(error) = commit_result {
        crate::util::report_setup_error(
            &env,
            &swap_id,
            format!("Protocol-5 commit failed: {error}"),
        );
        return;
    }

    // The redeem wallet scans from this height when the sweep happens, so it
    // must be recent but still before the XMR lock. The check wallet's cached
    // height is a hardcoded conservative constant (src/wallet.rs) and can be
    // thousands of blocks behind, which turns the sweep into a multi-minute
    // scan (WORKLOG §364). Ask the daemon for the chain tip instead; the lock
    // is created after this point, so a tip-based height is always safe.
    let height_started = std::time::Instant::now();
    let height_result = monero_wallet.block_height().await;
    tracing::info!(
        elapsed_ms = height_started.elapsed().as_millis() as u64,
        "V5_BOB_WALLET_HEIGHT_READ"
    );
    let mut restore_height = match height_result {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Failed to read Monero height before recording restore height: {error}"),
            );
            return;
        }
    };
    let recorded_height = restore_height.height as u64;
    let tip_started = std::time::Instant::now();
    let daemon_tip = match monero_daemon.as_ref() {
        Some(daemon) => match daemon.chain_height().await {
            Ok(height) => Some(height),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "Could not read the Monero daemon height; the redeem wallet will fall back to the wallet's cached height"
                );
                None
            }
        },
        None => None,
    };
    tracing::info!(
        elapsed_ms = tip_started.elapsed().as_millis() as u64,
        "V5_BOB_DAEMON_HEIGHT_READ"
    );
    let chosen_height = swap::monero::redeem_restore_height(daemon_tip, recorded_height);
    restore_height.height = chosen_height.min(u32::MAX as u64) as u32;
    tracing::info!(
        restore_height = restore_height.height,
        recorded_height,
        daemon_tip,
        "Recorded v5 Monero restore blockheight from the chain tip"
    );

    // The redeem needs the Monero output distribution (~17 MB over the whole
    // chain). Fetch or top it up now, while the swap waits for the locks, so the
    // redeem does not spend its first minutes downloading it.
    if let Some(daemon) = monero_daemon.clone() {
        tokio::spawn(async move { daemon.warm_output_distribution().await });
    }

    let (state3, _lock_tx) = match bob::prepare_lock(
        &*database,
        swap_uuid,
        state2,
        swap::protocol_v5::core::monero::BlockHeight {
            height: restore_height.height as u64,
        },
        &**bitcoin_wallet,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Failed to prepare Bitcoin lock: {error}"),
            );
            return;
        }
    };

    let lock_txid = match bob::publish_lock(&*database, swap_uuid, &bitcoin_wallet).await {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_post_lock_error(
                &env,
                &swap_id,
                format!("Failed to publish Bitcoin lock: {error}"),
            );
            return;
        }
    };
    util::on_btc_locked(&env, lock_txid.to_string());

    let state3_for_cancel = state3.clone();
    let state4 = loop {
        // The redeem retry below `continue`s back to here every 30 s while the
        // timelock is open. Without this check, stopping the swap did not stop
        // the loop as long as the timelock could be read (first review #6).
        if !util::get_running_swap(&env) {
            return;
        }
        match bob::run_until_signature_sent(
        &*database,
        &mut handle,
        swap_uuid,
        state3_for_cancel.clone(),
        |state4| {
            util::on_xmr_lock_proof_received(
                &env,
                swap_id.clone(),
                state4.lock_transfer_proof().tx_hash().0,
            )
        },
        |txid: &str, confirmations: u64| {
            util::on_xmr_lock_confirmation(&env, txid.to_string(), confirmations)
        },
        || util::on_btc_lock_tx_confirm(&env, lock_txid.to_string()),
        || {
            util::print_swap_log_ln(
                &env,
                format!("Alice's Bitcoin early refund was seen for swap {swap_id}; waiting for confirmation."),
            );
        },
        &**bitcoin_wallet,
        env_config.bitcoin_finality_confirmations,
        swap::protocol_v5::core::monero::BlockHeight {
            height: restore_height.height as u64,
        },
        &monero_wallet,
        monero_daemon.as_ref(),
        env_config.monero_network,
        env_config.monero_finality_confirmations,
    )
    .await
    {
        Ok(Some(value)) => break value,
        Ok(None) => {
            let state6 = state3_for_cancel.clone().cancel(swap::protocol_v5::core::monero::BlockHeight {
                height: restore_height.height as u64,
            });
            match bob::wait_for_early_refund_confirmation(&*database, swap_uuid, &state6, &**bitcoin_wallet).await {
                Ok(bob::EarlyRefundOutcome::CancelTimelockExpired) => crate::util::report_post_lock_error(&env, &swap_id, "Bitcoin early refund did not confirm before the cancel timelock; switching to cancel/refund.".to_string()),
                Ok(bob::EarlyRefundOutcome::Confirmed(txid)) => {
                    util::print_swap_log_ln(&env, format!("Bitcoin early refund transaction confirmed: {txid}"));
                    util::call_basic_listener_method(&env, swap_id.clone(), util::ON_SWAP_REFUNDED_METHOD);
                }
                Err(error) => crate::util::report_post_lock_error(&env, &swap_id, format!("Bitcoin early refund is published but not confirmed: {error}. Resume from History to wait for confirmation.")),
            }
            return;
        }
        Err(error) => {
            // Never cancel on a transient proof/ack/signature error. Legacy
            // stayed in the locked state until the cancel timelock actually
            // expired; v5 used to persist BtcCancelled and start the refund
            // immediately, which is what put the UI in limbo.
            // Bitcoin is locked, so a transient failure reading the timelock
            // (Electrum timeout, Tor stall) must not end the client: swap
            // f7a162f7 died here with both locks on-chain. Retry until the
            // user stops the swap; never cancel and never report fatal.
            let Some(timelock) = crate::util::retry_while_locked(
                || state3_for_cancel.expired_timelock(&**bitcoin_wallet),
                || util::get_running_swap(&env),
                |attempt, timelock_error, wait| {
                    let raw = format!("{timelock_error:#}");
                    tracing::warn!(attempt, error = %raw, "Cannot read the Bitcoin cancel timelock yet");
                    util::print_swap_log_ln(
                        &env,
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
                return;
            };
            match redeem_failure_action(&timelock, handle.accepted_state4().is_some()) {
                RedeemFailureAction::RetryRedeem => {
                    let raw = format!("{error:#}");
                    tracing::warn!(error = %raw, "Redeem not complete yet; retrying instead of canceling");
                    util::print_swap_log_ln(
                        &env,
                        format!(
                            "Redeem not complete yet: {}. Bitcoin is still locked; retrying instead of canceling.",
                            crate::util::humanize_error(&raw)
                        ),
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    continue;
                }
                RedeemFailureAction::ObserveOutcome => {
                    let Some(state4) = handle.accepted_state4() else {
                        return;
                    };
                    util::print_swap_log_ln(
                        &env,
                        "Bitcoin cancel timelock expired after Alice's Monero lock was accepted; checking whether Alice redeemed before cancelling."
                            .to_string(),
                    );
                    break state4;
                }
                RedeemFailureAction::CancelRefund => {}
            }
            util::print_swap_log_ln(
                &env,
                "Bitcoin cancel timelock expired before the redeem completed; starting cancel/refund."
                    .to_string(),
            );
            let state6 = match bob::cancel_lock(
                &*database,
                swap_uuid,
                state3_for_cancel,
                swap::protocol_v5::core::monero::BlockHeight {
                    height: restore_height.height as u64,
                },
            )
            .await
            {
                Ok(value) => value,
                Err(error) => {
                    crate::util::report_post_lock_error(
                        &env,
                        &swap_id,
                        format!("Failed to persist the Bitcoin cancel path: {error}"),
                    );
                    return;
                }
            };
            crate::util::notify_protocol_v5_cancel_prepared(&env, &swap_id);
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
                Err(error) => crate::util::report_cancel_refund_error(&env, &swap_id, "Failed to cancel/refund Bitcoin", &error),
            }
            return;
        }
    }
    };
    loop {
        if !util::get_running_swap(&env) {
            return;
        }
        match bob::observe_btc_outcome(&*database, swap_uuid, &state4, &**bitcoin_wallet).await {
            Ok(bob::BobOutcome::XmrRedeemed(state5)) => {
                // The GUI logs "Alice has redeemed the Bitcoin! Redeeming Monero..." for this callback.
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
                    Ok(_) => util::on_swap_completed(&env, swap_id.clone()),
                    Err(error) => crate::util::report_post_lock_error(
                        &env,
                        &swap_id,
                        format!("Failed to redeem XMR: {error}"),
                    ),
                }
                return;
            }
            Ok(bob::BobOutcome::Cancelled(state6)) => {
                util::print_swap_log_ln(
                    &env,
                    "Bitcoin cancel timelock expired; publishing cancel/refund"
                        .to_string(),
                );
                crate::util::notify_protocol_v5_cancel_prepared(&env, &swap_id);
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
                    Err(error) => crate::util::report_cancel_refund_error(&env, &swap_id, "Failed to cancel/refund Bitcoin", &error),
                }
                return;
            }
            Ok(bob::BobOutcome::Pending) => {}
            Err(error) => {
                tracing::warn!("Protocol-5 outcome poll failed: {error:#}");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

/// What a failure before the Bitcoin lock means to the user (quote, deposit wait, fresh quote).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum QuoteStageFailure {
    /// The user pressed Disconnect (or the client was stopped): never retry.
    UserDisconnected,
    /// A seller or network problem: the app may retry.
    Transient,
}

/// A stopped swap is a disconnect, not a quote error. The v1 flow downcast `SwapDisconnected`; the
/// v5 flow reported every error as "Error getting quote from seller. Retrying...", and the app then
/// restarted a swap the user had just cancelled (2026-11-14, cancelled at the deposit address).
pub(crate) fn classify_quote_stage_failure(user_stopped: bool, error: &anyhow::Error) -> QuoteStageFailure {
    if user_stopped || error.chain().any(|cause| cause.is::<SwapDisconnected>()) {
        QuoteStageFailure::UserDisconnected
    } else {
        QuoteStageFailure::Transient
    }
}

/// How much higher (worse for the buyer, in BTC per XMR) the seller's price may get between the
/// quote shown when the swap started and the quote asked again just before setup, in basis points.
/// Prices move while the deposit confirms; a bigger jump is refused, never accepted silently.
pub(crate) const FRESH_PRICE_TOLERANCE_BP: u64 = 200; // 2%

/// Refuses a fresh price (satoshi per XMR) that is more than [`FRESH_PRICE_TOLERANCE_BP`] higher
/// than the price the swap started with. A lower (better) price is always accepted.
pub(crate) fn check_fresh_price(first_sats: u64, fresh_sats: u64) -> Result<(), String> {
    if first_sats == 0 || fresh_sats == 0 {
        return Err("The seller quoted a price of zero, so the swap was stopped before anything was sent.".to_string());
    }
    let (first, fresh) = (first_sats as u128, fresh_sats as u128);
    if fresh * 10_000 > first * (10_000 + FRESH_PRICE_TOLERANCE_BP as u128) {
        let worse_bp = (fresh - first) * 10_000 / first;
        return Err(format!(
            "The seller's price got worse after the swap started: {} BTC per XMR then, {} BTC per XMR now ({}.{:02}% higher). \
             Nothing was sent to the seller and your Bitcoin is still in your swaps account. \
             Start the swap again to see the seller's current price, or pick another seller.",
            btc_string(first_sats),
            btc_string(fresh_sats),
            worse_bp / 100,
            worse_bp % 100
        ));
    }
    Ok(())
}

fn btc_string(sats: u64) -> String {
    format!("{}.{:08}", sats / 100_000_000, sats % 100_000_000)
}

fn report_quote_stage_failure(env: &JNIEnv<'_>, swap_id: &str, error: anyhow::Error) {
    match classify_quote_stage_failure(!util::get_running_swap(env), &error) {
        QuoteStageFailure::UserDisconnected => util::on_swap_error(
            env,
            SwapError {
                swap_id: swap_id.to_string(),
                error_type: ErrorType::SwapDisconnected,
                error_message: "Swap disconnected.".to_string(),
                fatal: true,
            },
        ),
        QuoteStageFailure::Transient => {
            util::on_generic_seller_quote_error(env, swap_id.to_string(), error.to_string())
        }
    }
}

#[cfg(test)]
mod quote_stage_tests {
    use super::*;

    #[test]
    fn a_stopped_swap_is_a_disconnect_whatever_the_error_says() {
        let error = anyhow::anyhow!("channel closed");
        assert_eq!(classify_quote_stage_failure(true, &error), QuoteStageFailure::UserDisconnected);
    }

    #[test]
    fn a_swap_disconnected_error_is_a_disconnect_even_wrapped_in_context() {
        let plain = anyhow::Error::new(SwapDisconnected);
        let wrapped = anyhow::Error::new(SwapDisconnected).context("while waiting for the deposit");
        assert_eq!(classify_quote_stage_failure(false, &plain), QuoteStageFailure::UserDisconnected);
        assert_eq!(classify_quote_stage_failure(false, &wrapped), QuoteStageFailure::UserDisconnected);
    }

    #[test]
    fn a_seller_or_network_failure_is_still_retried() {
        let error = anyhow::anyhow!("dial failed: Timeout has been reached");
        assert_eq!(classify_quote_stage_failure(false, &error), QuoteStageFailure::Transient);
    }

    /// Every quote-stage error site reports through the classifier (a raw generic report is what
    /// made a cancel look like a retry).
    #[test]
    fn no_quote_stage_site_reports_a_raw_generic_quote_error() {
        let source = include_str!("buy_xmr_v5.rs");
        let production = &source[..source.find("\n#[cfg(test)]\nmod quote_stage_tests").unwrap()];
        assert!(
            !production.contains("on_generic_seller_quote_error(&env, swap_id.clone()"),
            "report quote-stage failures with report_quote_stage_failure"
        );
        assert!(production.matches("report_quote_stage_failure(&env, &swap_id,").count() >= 4);
    }
}

#[cfg(test)]
mod fresh_quote_tests {
    /// Source check: `buyXmrV5` runs inside the JVM, so its wiring cannot be driven from
    /// a Rust test. The C-1 check itself is tested in `swap_setup` (unit and end to end).
    fn body() -> &'static str {
        let source = include_str!("buy_xmr_v5.rs");
        let start = source.find("fn Java_swap_client_ClientSwap_buyXmrV5").unwrap();
        &source[start..]
    }

    #[test]
    fn setup_gives_up_when_the_user_disconnects() {
        let body = body();
        let start = body.find("tokio::join!(bitcoin_init, monero_setup)").expect("setup join");
        let window = &body[start.saturating_sub(200)..(start + 900).min(body.len())];
        assert!(window.contains("wait_until_stopped"), "the setup join must be raced against a Disconnect");
        assert!(window.contains("report_quote_stage_failure"), "a stopped setup is reported as a disconnect");
    }

    #[test]
    fn the_quote_is_asked_for_again_after_the_deposit_wait() {
        let body = body();
        let deposit_wait = body.find("util::determine_btc_to_swap(").unwrap();
        let refresh = body.find("let fresh_quote = match handle.request_quote().await").unwrap();
        let setup = body.find(".start_swap(").unwrap();
        assert!(deposit_wait < refresh, "the refresh must come after the deposit wait");
        assert!(refresh < setup, "the refresh must come before setup");
    }

    #[test]
    fn setup_checks_against_the_fresh_price() {
        let body = body();
        let setup = &body[body.find(".start_swap(").unwrap()..];
        let new_swap = &setup[..setup.find("})").unwrap()];
        assert!(new_swap.contains("quoted_price: fresh_quote.price"), "{new_swap}");
    }
}

#[cfg(test)]
mod fresh_price_tests {
    use super::{check_fresh_price, FRESH_PRICE_TOLERANCE_BP};

    const PRICE: u64 = 500_000; // 0.005 BTC per XMR

    #[test]
    fn the_same_or_a_better_price_is_accepted() {
        assert!(check_fresh_price(PRICE, PRICE).is_ok());
        assert!(check_fresh_price(PRICE, PRICE / 2).is_ok());
    }

    #[test]
    fn a_small_move_within_the_margin_is_accepted() {
        let at_the_margin = PRICE + PRICE * FRESH_PRICE_TOLERANCE_BP / 10_000;
        assert!(check_fresh_price(PRICE, at_the_margin).is_ok());
    }

    #[test]
    fn a_price_just_over_the_margin_is_refused() {
        let over = PRICE + PRICE * FRESH_PRICE_TOLERANCE_BP / 10_000 + 1;
        assert!(check_fresh_price(PRICE, over).is_err());
    }

    #[test]
    fn a_bait_and_switch_price_is_refused_with_both_prices_in_the_message() {
        let error = check_fresh_price(PRICE, PRICE * 10).unwrap_err();
        assert!(error.contains("0.00500000 BTC per XMR then"), "{error}");
        assert!(error.contains("0.05000000 BTC per XMR now"), "{error}");
        assert!(error.contains("900.00% higher"), "{error}");
        assert!(error.contains("Nothing was sent"), "{error}");
    }

    #[test]
    fn a_zero_price_is_refused() {
        assert!(check_fresh_price(0, PRICE).is_err());
        assert!(check_fresh_price(PRICE, 0).is_err());
    }

    /// Source check (the wiring runs inside the JVM): the price is checked after the refresh and
    /// before setup, and a refusal is a setup error, which the GUI never restarts on its own.
    #[test]
    fn the_fresh_price_is_checked_before_setup_and_stops_the_swap() {
        let source = include_str!("buy_xmr_v5.rs");
        let body = &source[source.find("fn Java_swap_client_ClientSwap_buyXmrV5").unwrap()..];
        let refresh = body.find("let fresh_quote = match handle.request_quote().await").unwrap();
        let check = body.find("check_fresh_price(quote.price.to_sat(), fresh_quote.price.to_sat())").unwrap();
        let setup = body.find(".start_swap(").unwrap();
        assert!(refresh < check && check < setup, "the check must sit between the refresh and setup");
        let after_check = &body[check..setup];
        assert!(after_check.contains("crate::util::report_setup_error(&env, &swap_id, reason)"), "a refusal must be a setup error");
        assert!(after_check.contains("return;"), "a refusal must stop the swap");
    }
}

#[cfg(test)]
mod redeem_failure_policy_tests {
    use super::{redeem_failure_action, RedeemFailureAction};
    use swap::protocol_v5::core::bitcoin::ExpiredTimelocks;

    #[test]
    fn missing_timelock_expiry_retries_redeem() {
        let timelock = ExpiredTimelocks::None { blocks_left: 12 };
        assert_eq!(
            redeem_failure_action(&timelock, false),
            RedeemFailureAction::RetryRedeem,
            "a transient redeem error before the cancel timelock must not cancel"
        );
    }

    #[test]
    fn expired_cancel_timelock_allows_cancel_refund() {
        let timelock = ExpiredTimelocks::Cancel { blocks_left: 0 };
        assert_eq!(
            redeem_failure_action(&timelock, false),
            RedeemFailureAction::CancelRefund
        );
    }

    #[test]
    fn punish_timelock_allows_cancel_refund_path() {
        let timelock = ExpiredTimelocks::Punish;
        assert_eq!(
            redeem_failure_action(&timelock, false),
            RedeemFailureAction::CancelRefund
        );
    }

    #[test]
    fn expired_timelock_after_the_lock_was_accepted_follows_the_chain_instead_of_cancelling() {
        for timelock in [
            ExpiredTimelocks::Cancel { blocks_left: 0 },
            ExpiredTimelocks::Punish,
        ] {
            assert_eq!(
                redeem_failure_action(&timelock, true),
                RedeemFailureAction::ObserveOutcome
            );
        }
    }

    #[test]
    fn open_timelock_retries_even_after_the_lock_was_accepted() {
        let timelock = ExpiredTimelocks::None { blocks_left: 3 };
        assert_eq!(
            redeem_failure_action(&timelock, true),
            RedeemFailureAction::RetryRedeem
        );
    }
}


/// Pacing for the deposit-wait loop.
///
/// The loop calls its sync closure back to back. A full Electrum sync takes
/// 60-100s over Tor, so a deposit that lands mid-sync used to wait out that
/// sync plus the next one (about 4 minutes in practice). Dojo answers in a few
/// seconds and already knows the unconfirmed deposit, so it is polled often and
/// the full Electrum sync only runs occasionally.
pub(crate) struct DepositSyncState {
    last_dojo: std::sync::Mutex<Option<std::time::Instant>>,
    last_full: std::sync::Mutex<Option<std::time::Instant>>,
}

pub(crate) const DEPOSIT_DOJO_POLL: std::time::Duration = std::time::Duration::from_secs(5);
/// How long one Electrum sync inside the deposit-wait loop may run. Once the
/// wallet is open an idle sync takes ~10 s, and Dojo is the primary detector, so
/// a stalled server must not hold the loop for the pool's 150 s deadline.
pub(crate) const DEPOSIT_FULL_SYNC_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
pub(crate) const DEPOSIT_FULL_SYNC_EVERY: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DepositSyncStep {
    /// Too soon for any network work.
    Skip,
    /// Ask Dojo for new unconfirmed deposits.
    Dojo,
    /// Run the full Electrum sync (Dojo failed, or it is due).
    Full,
}

pub(crate) fn deposit_sync_step(
    since_dojo: Option<std::time::Duration>,
    since_full: Option<std::time::Duration>,
) -> DepositSyncStep {
    if since_full.map_or(true, |d| d >= DEPOSIT_FULL_SYNC_EVERY) {
        return DepositSyncStep::Full;
    }
    if since_dojo.map_or(true, |d| d >= DEPOSIT_DOJO_POLL) {
        return DepositSyncStep::Dojo;
    }
    DepositSyncStep::Skip
}

impl DepositSyncState {
    pub(crate) fn new() -> Self {
        // The wallet was fully synced when it was opened.
        Self {
            last_dojo: std::sync::Mutex::new(None),
            last_full: std::sync::Mutex::new(Some(std::time::Instant::now())),
        }
    }
}

/// Runs `sync`, giving up after `budget`. A timed-out sync is not an error: the
/// loop just checks again with the wallet state it already has.
pub(crate) async fn bounded_deposit_sync<F>(
    sync: F,
    budget: std::time::Duration,
) -> anyhow::Result<()>
where
    F: std::future::Future<Output = anyhow::Result<()>>,
{
    match tokio::time::timeout(budget, sync).await {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!(
                budget_secs = budget.as_secs(),
                "Electrum sync did not finish in time while waiting for the deposit; will check again"
            );
            Ok(())
        }
    }
}

async fn deposit_wait_sync(
    wallet: &swap::bitcoin::Wallet,
    state: &DepositSyncState,
) -> anyhow::Result<()> {
    let since = |m: &std::sync::Mutex<Option<std::time::Instant>>| {
        m.lock().ok().and_then(|g| g.map(|t| t.elapsed()))
    };
    let step = deposit_sync_step(since(&state.last_dojo), since(&state.last_full));
    let mark = |m: &std::sync::Mutex<Option<std::time::Instant>>| {
        if let Ok(mut g) = m.lock() {
            *g = Some(std::time::Instant::now());
        }
    };
    match step {
        DepositSyncStep::Skip => Ok(()),
        DepositSyncStep::Dojo => {
            mark(&state.last_dojo);
            match wallet.seed_from_dojo().await {
                Ok(_) => Ok(()),
                Err(error) => {
                    tracing::warn!(%error, "Dojo deposit check failed; falling back to the Electrum sync");
                    mark(&state.last_full);
                    bounded_deposit_sync(wallet.sync(), DEPOSIT_FULL_SYNC_BUDGET).await
                }
            }
        }
        DepositSyncStep::Full => {
            mark(&state.last_dojo);
            mark(&state.last_full);
            bounded_deposit_sync(wallet.sync(), DEPOSIT_FULL_SYNC_BUDGET).await
        }
    }
}

#[cfg(test)]
mod deposit_sync_tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_stalled_sync_is_abandoned_after_the_budget() {
        let started = std::time::Instant::now();
        let result = bounded_deposit_sync(std::future::pending::<anyhow::Result<()>>(), Duration::from_millis(50)).await;
        assert!(result.is_ok());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_failed_sync_inside_the_budget_still_reports_its_error() {
        let result = bounded_deposit_sync(async { Err(anyhow::anyhow!("boom")) }, Duration::from_secs(30)).await;
        assert!(result.is_err());
    }

    #[test]
    fn right_after_a_full_sync_dojo_is_polled_first() {
        assert_eq!(deposit_sync_step(None, Some(Duration::from_secs(1))), DepositSyncStep::Dojo);
    }

    #[test]
    fn dojo_is_not_hammered() {
        assert_eq!(
            deposit_sync_step(Some(Duration::from_secs(1)), Some(Duration::from_secs(2))),
            DepositSyncStep::Skip
        );
    }

    #[test]
    fn dojo_is_polled_again_after_its_interval() {
        assert_eq!(
            deposit_sync_step(Some(Duration::from_secs(6)), Some(Duration::from_secs(10))),
            DepositSyncStep::Dojo
        );
    }

    #[test]
    fn a_full_sync_is_still_run_periodically() {
        assert_eq!(
            deposit_sync_step(Some(Duration::from_secs(1)), Some(Duration::from_secs(121))),
            DepositSyncStep::Full
        );
    }
}
