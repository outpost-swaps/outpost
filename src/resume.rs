use std::str::FromStr;
use std::sync::Arc;

use jni::objects::JClass;
use jni::sys::jstring;
use jni::JNIEnv;
use rand::Rng;
use swap::cli::EventLoop;
use swap::env::{GetConfig, Mainnet, Testnet};
use swap::network::rendezvous::XmrBtcNamespace;
use swap::network::swarm;
use swap::protocol::bob::{BobState, Swap};
use swap::swap_error::{ErrorType, SwapError};
use swap::{cli, fs, util};

use url::Url;
use uuid::Uuid;

use crate::model::resume_request::ResumeRequest;
use crate::wallet;

#[tokio::main]
#[no_mangle]
#[warn(unused_variables)]
pub async extern "system" fn Java_swap_client_ClientSwap_resume(
    env: JNIEnv,
    _class: JClass,
    resume_request_json: jstring,
) {
    let resume_request_json_string = match util::get_string_value(&env, resume_request_json) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to read resume request");
            return;
        }
    };
    let resume_request: ResumeRequest =
        match serde_json::from_str(resume_request_json_string.as_str()) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(?error, "Failed to parse resume request");
                return;
            }
        };
    let swap_id = resume_request.swap_id.clone();
    let swap_uuid = match Uuid::from_str(resume_request.swap_id.as_str()) {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(&env, &swap_id, format!("Invalid swap id: {error}"));
            return;
        }
    };
    let swap_id = swap_uuid.to_string();

    let bitcoin_electrum_rpc_url = match Url::from_str(resume_request.electrum_url.as_str()) {
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

    /* Constants */
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
    let env_config;
    let namespace;
    if resume_request.testnet {
        env_config = Testnet::get_config();
        namespace = XmrBtcNamespace::Testnet;
    } else {
        env_config = Mainnet::get_config();
        namespace = XmrBtcNamespace::Mainnet;
    }

    let seed = match crate::util::decode_seed(&resume_request.seed_base64) {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(&env, &swap_id, format!("Invalid seed: {error}"));
            return;
        }
    };

    #[cfg(feature = "protocol-v5")]
    {
        let data_dir_for_v5 = match fs::system_data_dir() {
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
        let database_for_v5 = match swap::database::SqliteDatabase::open_with_cipher(
            data_dir_for_v5.join("sqlite"),
            Some(swap::database::StateCipher::from_seed(&seed)),
        )
        .await
        {
            Ok(value) => value,
            Err(_) => {
                // Let the v5 path report the concrete database-open error.
                crate::resume_v5::resume(
                    env,
                    resume_request,
                    seed,
                    env_config,
                    data_dir_for_v5,
                    swap_uuid,
                )
                .await;
                return;
            }
        };

        let has_v5_state = database_for_v5
            .load_protocol_v5_state::<swap::protocol_v5::db::bob::Bob>(swap_uuid, "bob")
            .await
            .ok()
            .flatten()
            .is_some();
        if has_v5_state {
            crate::resume_v5::resume(
                env,
                resume_request,
                seed,
                env_config,
                data_dir_for_v5,
                swap_uuid,
            )
            .await;
            return;
        }
    }
    let db = match swap::database::open_db_with_cipher(
        &data_dir.join("sqlite"),
        Some(swap::database::StateCipher::from_seed(&seed)),
    )
    .await
    {
        Ok(val) => val,
        Err(error) => {
            let swap_error = SwapError {
                swap_id: swap_id.clone(),
                error_type: ErrorType::FailedToLoadSwapDatabase,
                error_message: format!("Failed to load swap database: {error}"),
                fatal: true,
            };
            util::on_swap_error(&env, swap_error);
            return;
        }
    };

    // Persist the swap client's own logs (tracing) so resume failures remain
    // diagnosable after the fact. A second init in the same process fails
    // harmlessly and is ignored.
    let _ = std::fs::create_dir_all(data_dir.join("logs"));
    let _ = cli::tracing::init(false, false, data_dir.join("logs"), Some(swap_uuid));

    let tmp_folder_num = rand::thread_rng().gen_range(0..25565);
    let tmp_data_dir = data_dir.join("tmp");
    let tmp_deposit_folder = tmp_data_dir.join(format!(".tmp_deposit_{}", tmp_folder_num));
    util::print_swap_log_ln(
        &env,
        "[SWAP CLIENT] Setting up Bitcoin wallet...".to_string(),
    );
    let bitcoin_wallet = match wallet::init_bitcoin_wallet(
        bitcoin_electrum_rpc_url.clone(),
        &resume_request.electrum_pool_urls,
        resume_request.proxy.as_str(),
        &seed,
        tmp_deposit_folder,
        env_config,
        resume_request.swaps_receive_index,
        resume_request.swaps_change_index,
        resume_request.dojo_url,
        resume_request.dojo_api_key,
    )
    .await
    {
        Ok(val) => val,
        Err(err) => {
            let swap_error = SwapError {
                swap_id: swap_uuid.to_string(),
                error_type: ErrorType::FailedToInitBitcoinWallet,
                error_message: err.to_string(),
                fatal: true,
            };
            util::on_swap_error(&env, swap_error);
            return;
        }
    };
    if !util::get_running_swap(&env) {
        let swap_error = SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::SwapDisconnected,
            error_message: "".to_string(),
            fatal: true,
        };
        util::on_swap_error(&env, swap_error);
        return;
    }
    util::print_swap_log_ln(&env, "[SWAP CLIENT] Initialized Bitcoin wallet".to_string());
    util::print_swap_log_ln(
        &env,
        "[SWAP CLIENT] Setting up Monero wallet...".to_string(),
    );
    let xmr_wallet_name = format!("{}-monitoring-wallet", swap_uuid.to_string());
    let monero_wallet = match wallet::init_monero_wallet(
        resume_request.xmr_rpc_endpoint,
        env_config,
        xmr_wallet_name.as_str(),
    )
    .await
    {
        Ok(val) => val,
        Err(err) => {
            let swap_error = SwapError {
                swap_id: swap_uuid.to_string(),
                error_type: ErrorType::FailedToInitMoneroWallet,
                error_message: err.to_string(),
                fatal: true,
            };
            util::on_swap_error(&env, swap_error);
            return;
        }
    };
    if !util::get_running_swap(&env) {
        let swap_error = SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::SwapDisconnected,
            error_message: "".to_string(),
            fatal: true,
        };
        util::on_swap_error(&env, swap_error);
        return;
    }
    util::print_swap_log_ln(
        &env,
        "[SWAP CLIENT] Initialized Monero monitoring wallet".to_string(),
    );

    let bitcoin_wallet = Arc::new(bitcoin_wallet);
    // Per-swap lookups must fail as a swap error, not panic across JNI: a panic
    // here is undefined behavior and takes down the whole process.
    let seller_peer_id = match db.get_peer_id(swap_uuid).await {
        Ok(peer_id) => peer_id,
        Err(error) => {
            let swap_error = SwapError {
                swap_id: swap_uuid.to_string(),
                error_type: ErrorType::UnknownError,
                error_message: format!(
                    "Swap is missing its seller peer record and cannot be resumed: {error}"
                ),
                fatal: true,
            };
            util::on_swap_error(&env, swap_error);
            return;
        }
    };
    let seller_addresses = match db.get_addresses(seller_peer_id).await {
        Ok(addresses) if !addresses.is_empty() => addresses,
        Ok(_) => {
            let swap_error = SwapError {
                swap_id: swap_uuid.to_string(),
                error_type: ErrorType::UnknownError,
                error_message: "Swap has no seller addresses recorded and cannot be resumed."
                    .to_string(),
                fatal: true,
            };
            util::on_swap_error(&env, swap_error);
            return;
        }
        Err(error) => {
            let swap_error = SwapError {
                swap_id: swap_uuid.to_string(),
                error_type: ErrorType::UnknownError,
                error_message: format!("Failed to load seller addresses during resume: {error}"),
                fatal: true,
            };
            util::on_swap_error(&env, swap_error);
            return;
        }
    };
    let seller_address = match seller_addresses.first() {
        Some(value) => value.clone(),
        None => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                "Swap has no seller addresses recorded and cannot be resumed.".to_string(),
            );
            return;
        }
    };
    let behaviour = cli::Behaviour::new(
        seller_peer_id,
        env_config,
        bitcoin_wallet.clone(),
        (seed.derive_libp2p_identity(), namespace),
    );
    let mut swarm = match swarm::cli(
        seed.derive_libp2p_identity(),
        resume_request.proxy_port,
        behaviour,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(
                &env,
                &swap_id,
                format!("Failed to start network: {error}"),
            );
            return;
        }
    };
    if let Err(e) = swarm.listen_on(
        "/ip4/127.0.0.1/tcp/0"
            .parse()
            .expect("valid loopback multiaddr"),
    ) {
        tracing::warn!(%e, "failed to listen on loopback; seller return path falls back to Tor circuit addresses");
    }
    for seller_address in &seller_addresses {
        swarm.add_peer_address(seller_peer_id, seller_address.clone());
    }
    swarm.add_peer_address(
        seller_peer_id,
        "/ip4/127.0.0.1/tcp/9939"
            .parse()
            .expect("valid loopback multiaddr"),
    );

    let (event_loop, event_loop_handle) =
        match EventLoop::new(swap_uuid, swarm, seller_peer_id, db.clone()) {
            Ok(value) => value,
            Err(error) => {
                crate::util::report_setup_error(
                    &env,
                    &swap_id,
                    format!("Failed to start swap event loop: {error}"),
                );
                return;
            }
        };
    let _handle = tokio::spawn(event_loop.run());
    let monero_receive_address = match db.get_monero_address(swap_uuid).await {
        Ok(address) => address,
        Err(error) => {
            let swap_error = SwapError {
                swap_id: swap_uuid.to_string(),
                error_type: ErrorType::UnknownError,
                error_message: format!(
                    "Swap is missing its XMR receive address and cannot be resumed: {error}"
                ),
                fatal: true,
            };
            util::on_swap_error(&env, swap_error);
            return;
        }
    };
    let swap = match Swap::from_db(
        db,
        swap_uuid,
        bitcoin_wallet,
        Arc::new(monero_wallet),
        env_config,
        event_loop_handle,
        monero_receive_address,
    )
    .await
    {
        Ok(swap) => swap,
        Err(error) => {
            let swap_error = SwapError {
                swap_id: swap_uuid.to_string(),
                error_type: ErrorType::UnknownError,
                error_message: format!("Failed to load swap state during resume: {error:#}"),
                fatal: true,
            };
            util::on_swap_error(&env, swap_error);
            return;
        }
    };

    let (resume_btc_sats, resume_xmr_piconero) = resume_display_amounts(&swap.state);
    util::on_swap_resume_context(
        &env,
        swap.id.to_string(),
        seller_address.clone(),
        swap.monero_receive_address,
        resume_btc_sats,
        resume_xmr_piconero,
    );

    match crate::swap_state_manager::run(seller_address.clone(), swap, &env).await {
        Ok(bobstate) => match bobstate {
            BobState::BtcPunished { tx_lock_id: _ } => {
                let swap_error = SwapError {
                    swap_id: swap_uuid.to_string(),
                    error_type: ErrorType::SwapPunished,
                    error_message: "You have been punished for not refunding in time.".to_string(),
                    fatal: true,
                };
                util::on_swap_error(&env, swap_error);
            }
            BobState::BtcRefunded(_state6) => {
                util::call_basic_listener_method(
                    &env,
                    swap_uuid.to_string(),
                    util::ON_SWAP_REFUNDED_METHOD,
                );
            }
            BobState::SafelyAborted => {
                util::call_basic_listener_method(
                    &env,
                    swap_uuid.to_string(),
                    util::ON_SWAP_SAFELY_ABORTED_METHOD,
                );
            }
            BobState::XmrRedeemed { tx_lock_id: _ } => {
                util::on_swap_completed(&env, swap_uuid.to_string());
            }
            _ => {
                println!("{}", format!("OTHER STATE"))
            }
        },
        Err(error) => {
            let swap_error = crate::util::map_swap_completion_error(error, swap_uuid);
            util::on_swap_error(&env, swap_error);
        }
    }
}

fn resume_display_amounts(state: &BobState) -> (u64, u64) {
    match state {
        BobState::Started { btc_amount, .. } => (btc_amount.to_sat(), 0),
        BobState::SwapSetupCompleted(state2) => (
            state2.btc_amount().to_sat(),
            state2.xmr_amount().as_piconero(),
        ),
        BobState::BtcLockSigned { state3, .. }
        | BobState::BtcLocked { state3, .. }
        | BobState::XmrLockProofReceived { state: state3, .. } => (
            state3.btc_amount().to_sat(),
            state3.xmr_amount().as_piconero(),
        ),
        BobState::XmrLocked(state4) | BobState::EncSigSent(state4) => {
            (state4.btc_amount().to_sat(), 0)
        }
        BobState::BtcRedeemed(state5) => (state5.btc_amount().to_sat(), 0),
        BobState::CancelTimelockExpired(state6)
        | BobState::BtcCancelled(state6)
        | BobState::BtcRefunded(state6) => (state6.btc_amount().to_sat(), 0),
        _ => (0, 0),
    }
}
