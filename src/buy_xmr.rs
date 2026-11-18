use std::str::FromStr;
use std::sync::Arc;

use jni::objects::JClass;
use jni::sys::jstring;
use jni::JNIEnv;
use libp2p::Multiaddr;
use monero::Address;
use swap::bitcoin::TxLock;
use swap::cli::EventLoop;
use swap::env::{GetConfig, Mainnet, Testnet};
use swap::libp2p_ext::MultiAddrExt;
use swap::network::quote::{SwapDisconnected, ZeroQuoteReceived};
use swap::network::rendezvous::XmrBtcNamespace;
use swap::network::swarm;
use swap::protocol::bob::{BobState, Swap};
use swap::swap_error::{ErrorType, SwapError};
use swap::{cli, fs, util};

use url::Url;
use uuid::Uuid;

use crate::model::swap_request::SwapRequest;
use crate::wallet;

#[tokio::main]
#[no_mangle]
#[warn(unused_variables)]
pub async extern "system" fn Java_swap_client_ClientSwap_buyXmr(
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
    let swap_request: SwapRequest = match serde_json::from_str(swap_request_json_string.as_str()) {
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
    if swap_request.testnet {
        env_config = Testnet::get_config();
        namespace = XmrBtcNamespace::Testnet;
    } else {
        env_config = Mainnet::get_config();
        namespace = XmrBtcNamespace::Mainnet;
    }

    let seed = match crate::util::decode_seed(&swap_request.seed_base64) {
        Ok(value) => value,
        Err(error) => {
            crate::util::report_setup_error(&env, &swap_id, format!("Invalid seed: {error}"));
            return;
        }
    };
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
    // Persist the swap client's own logs (tracing) so failures in the
    // quote/deposit-wait phase — which happens before any protocol state is
    // persisted — remain diagnosable after the fact. A second init in the same
    // process (concurrent swaps) fails harmlessly and is ignored.
    let _ = std::fs::create_dir_all(data_dir.join("logs"));
    let _ = cli::tracing::init(false, false, data_dir.join("logs"), Some(swap_uuid));

    // Fail closed on a mismatched XMR destination network: a mainnet address in
    // stagenet mode (or vice versa) must never become the swap's XMR destination.
    if monero_receive_address.network != env_config.monero_network {
        let swap_error = SwapError {
            swap_id: swap_uuid.to_string(),
            error_type: ErrorType::UnknownError,
            error_message: format!(
                "The provided Monero receive address is a {:?} address but the app is set to {:?}. Refusing to start the swap with a mismatched destination.",
                monero_receive_address.network, env_config.monero_network
            ),
            fatal: true
        };
        util::on_swap_error(&env, swap_error);
        return;
    }

    let tmp_data_dir = data_dir.join("tmp");
    let tmp_deposit_folder = tmp_data_dir.join(format!(".tmp_deposit_{}", swap_uuid.to_string()));
    util::print_swap_log_ln(
        &env,
        "[SWAP CLIENT] Setting up Bitcoin wallet...".to_string(),
    );
    let bitcoin_wallet = match wallet::init_bitcoin_wallet(
        bitcoin_electrum_rpc_url.clone(),
        &swap_request.electrum_pool_urls,
        swap_request.proxy.as_str(),
        &seed,
        tmp_deposit_folder,
        env_config,
        swap_request.swaps_receive_index,
        swap_request.swaps_change_index,
        swap_request.dojo_url,
        swap_request.dojo_api_key,
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
        swap_request.xmr_rpc_endpoint,
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
    util::print_swap_log_ln(&env, "[SWAP CLIENT] Initialized Monero wallet".to_string());
    let bitcoin_wallet = Arc::new(bitcoin_wallet);

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
    if let Err(error) = db.insert_address(seller_peer_id, seller.clone()).await {
        crate::util::report_setup_error(
            &env,
            &swap_id,
            format!("Failed to save seller address: {error}"),
        );
        return;
    }

    let behaviour = cli::Behaviour::new(
        seller_peer_id,
        env_config,
        bitcoin_wallet.clone(),
        (seed.derive_libp2p_identity(), namespace),
    );
    let mut swarm = match swarm::cli(
        seed.derive_libp2p_identity(),
        swap_request.proxy_port,
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
    swarm.add_peer_address(seller_peer_id, seller.clone());
    swarm.add_peer_address(
        seller_peer_id,
        "/ip4/127.0.0.1/tcp/9939"
            .parse()
            .expect("valid loopback multiaddr"),
    );

    let (event_loop, mut event_loop_handle) =
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
    let max_givable = || bitcoin_wallet.max_giveable(TxLock::script_size());
    let estimate_fee = |amount| bitcoin_wallet.estimate_fee_vsize(TxLock::vsize(), amount);

    let (amount, _fees) = match util::determine_btc_to_swap(
        swap_uuid,
        event_loop_handle.request_quote(),
        bitcoin_wallet.new_address(),
        || bitcoin_wallet.balance(),
        max_givable,
        || bitcoin_wallet.sync(),
        estimate_fee,
        &env,
    )
    .await
    {
        Ok(val) => val,
        Err(error) => {
            match error.downcast::<ZeroQuoteReceived>() {
                Ok(_) => {
                    let swap_error = SwapError {
                    swap_id: swap_uuid.to_string(),
                    error_type: ErrorType::SellerInsufficientBalance,
                    error_message: "Seller does not have sufficient Monero for swap. Please check back later.".to_string(),
                    fatal: true
                };
                    util::on_swap_error(&env, swap_error);
                    return;
                }
                Err(other) => match other.downcast::<SwapDisconnected>() {
                    Ok(_) => {
                        let swap_error = SwapError {
                            swap_id: swap_uuid.to_string(),
                            error_type: ErrorType::SwapDisconnected,
                            error_message: "Swap disconnected.".to_string(),
                            fatal: true,
                        };
                        util::on_swap_error(&env, swap_error);
                        return;
                    }
                    Err(generic_error) => {
                        util::on_generic_seller_quote_error(
                            &env,
                            swap_uuid.to_string(),
                            generic_error.to_string(),
                        );
                        return;
                    }
                },
            }
        }
    };

    util::on_swap_amount_determined(&env, amount);

    if let Err(error) = db.insert_peer_id(swap_uuid, seller_peer_id).await {
        crate::util::report_setup_error(
            &env,
            &swap_id,
            format!("Failed to save seller peer ID: {error}"),
        );
        return;
    }
    if let Err(error) = db
        .insert_monero_address(swap_uuid, monero_receive_address)
        .await
    {
        crate::util::report_setup_error(
            &env,
            &swap_id,
            format!("Failed to save Monero address: {error}"),
        );
        return;
    }

    let swap = Swap::new(
        db,
        swap_uuid,
        bitcoin_wallet,
        Arc::new(monero_wallet),
        env_config,
        event_loop_handle,
        monero_receive_address,
        bitcoin_refund_address,
        amount,
    );

    match crate::swap_state_manager::run(seller.clone(), swap, &env).await {
        Ok(bobstate) => match bobstate {
            BobState::BtcPunished { tx_lock_id: _ } => {
                let swap_error = SwapError {
                        swap_id: swap_uuid.to_string(),
                        error_type: ErrorType::SwapPunished,
                        error_message: "You have been punished for not refunding in time. The seller has received the Bitcoin, while the Monero remains locked forever.".to_string(),
                        fatal: true
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
