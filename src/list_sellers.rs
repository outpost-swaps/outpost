use std::str::FromStr;

use jni::objects::JClass;
use jni::sys::jstring;
use jni::JNIEnv;
use libp2p::Multiaddr;

use swap::libp2p_ext::MultiAddrExt;
use swap::network::rendezvous::XmrBtcNamespace;
use swap::util;

use crate::internal::internal_list_sellers;
use crate::internal::internal_list_sellers::internal_list_sellers;
use crate::internal::internal_list_sellers::internal_list_sellers_direct;
use crate::model::list_sellers_direct_request::ListSellersDirectRequest;
use crate::model::list_sellers_request::ListSellersRequest;
use crate::model::list_sellers_result::ListSellersResult;
use crate::model::seller_data;
use crate::model::seller_data::SellerData;

#[tokio::main]
#[no_mangle]
#[warn(unused_variables)]
pub async extern "system" fn Java_swap_lib_AppSwap_listSellers(
    env: JNIEnv,
    _class: JClass,
    list_sellers_request_json: jstring,
) -> jstring {
    let list_sellers_request_json_string =
        match util::get_string_value(&env, list_sellers_request_json) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(?error, "Failed to read list sellers request");
                return empty_sellers(&env);
            }
        };
    let list_sellers_request: ListSellersRequest =
        match serde_json::from_str(list_sellers_request_json_string.as_str()) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(?error, "Failed to parse list sellers request");
                return empty_sellers(&env);
            }
        };

    let rendezvous_peer =
        match Multiaddr::from_str(list_sellers_request.libp2p_rendezvous_address.as_str()) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(?error, "Failed to parse rendezvous multiaddr");
                return empty_sellers(&env);
            }
        };

    if let Err(error) = crate::util::decode_seed(&list_sellers_request.seed_base64) {
        tracing::error!(?error, "Failed to decode seller list seed");
        return empty_sellers(&env);
    }
    // Seller discovery is anonymous and must not share the wallet's libp2p
    // identity with the swap swarm. Sharing it lets concurrent seller-list
    // connections exhaust the maker's per-peer connection limit and starves
    // the actual swap connection.
    let identity = libp2p::identity::Keypair::generate_ed25519();
    let rendezvous_peer_id = match rendezvous_peer.extract_peer_id() {
        Some(value) => value,
        None => {
            tracing::error!("Rendezvous address is missing a peer ID");
            return empty_sellers(&env);
        }
    };

    let namespace = if list_sellers_request.testnet {
        XmrBtcNamespace::Testnet
    } else {
        XmrBtcNamespace::Mainnet
    };

    let sellers = match internal_list_sellers(
        rendezvous_peer_id,
        rendezvous_peer,
        namespace,
        list_sellers_request.proxy_port,
        identity,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to list sellers");
            return empty_sellers(&env);
        }
    };

    let result = ListSellersResult {
        sellers: sellers
            .into_iter()
            .map(|seller| SellerData {
                multiaddr: seller.multiaddr.to_string(),
                status: match seller.status {
                    internal_list_sellers::Status::Online(quote) => seller_data::Status {
                        offline: false,
                        price: quote.price.to_btc().to_string(),
                        min_quantity: quote.min_quantity.to_btc().to_string(),
                        max_quantity: quote.max_quantity.to_btc().to_string(),
                        refund_policy: refund_policy_label(&quote.refund_policy),
                    },
                    internal_list_sellers::Status::Unreachable => seller_data::Status {
                        offline: true,
                        price: "".to_string(),
                        min_quantity: "".to_string(),
                        max_quantity: "".to_string(),
                        refund_policy: "".to_string(),
                    },
                },
            })
            .rev()
            .collect(),
    };
    sellers_result_to_jstring(&env, &result)
}

fn refund_policy_label(policy: &swap::network::quote::RefundPolicyWire) -> String {
    let refund_percent = match policy {
        swap::network::quote::RefundPolicyWire::FullRefund => 100.0,
        swap::network::quote::RefundPolicyWire::PartialRefund {
            anti_spam_deposit_ratio,
        } => {
            let ratio = anti_spam_deposit_ratio.as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
            (1.0 - ratio) * 100.0
        }
    };
    // Trim trailing zeros: 100.00 -> "100%", 98.50 -> "98.5%".
    let text = format!("{refund_percent:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text}%")
}
fn empty_sellers(env: &JNIEnv<'_>) -> jstring {
    match env.new_string(r#"{"sellers":[]}"#) {
        Ok(response) => response.into_inner(),
        Err(_) => std::ptr::null_mut(),
    }
}

fn sellers_result_to_jstring(env: &JNIEnv<'_>, result: &ListSellersResult) -> jstring {
    match serde_json::to_string(result) {
        Ok(response_json) => match env.new_string(response_json) {
            Ok(response) => response.into_inner(),
            Err(error) => {
                tracing::error!(?error, "Failed to allocate sellers response string");
                std::ptr::null_mut()
            }
        },
        Err(error) => {
            tracing::error!(?error, "Failed to serialize sellers response");
            empty_sellers(env)
        }
    }
}
#[tokio::main]
#[no_mangle]
pub async extern "system" fn Java_swap_lib_AppSwap_listSellersDirect(
    env: JNIEnv,
    _class: JClass,
    list_sellers_direct_request_json: jstring,
) -> jstring {
    let request_json_string = match util::get_string_value(&env, list_sellers_direct_request_json) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to read direct seller list request");
            return empty_sellers(&env);
        }
    };
    let request: ListSellersDirectRequest = match serde_json::from_str(request_json_string.as_str())
    {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to parse direct seller list request");
            return empty_sellers(&env);
        }
    };

    if let Err(error) = crate::util::decode_seed(&request.seed_base64) {
        tracing::error!(?error, "Failed to decode direct seller list seed");
        return empty_sellers(&env);
    }
    let identity = libp2p::identity::Keypair::generate_ed25519();

    let maker_addresses: Vec<Multiaddr> = request
        .maker_addresses
        .iter()
        .filter_map(|address| match Multiaddr::from_str(address.as_str()) {
            Ok(multiaddr) => Some(multiaddr),
            Err(error) => {
                eprintln!("Skipping malformed known maker address: {error}");
                None
            }
        })
        .collect();

    let sellers =
        match internal_list_sellers_direct(maker_addresses, request.proxy_port, identity).await {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(?error, "Failed to list sellers directly");
                return empty_sellers(&env);
            }
        };

    let result = ListSellersResult {
        sellers: sellers
            .into_iter()
            .map(|seller| SellerData {
                multiaddr: seller.multiaddr.to_string(),
                status: match seller.status {
                    internal_list_sellers::Status::Online(quote) => seller_data::Status {
                        offline: false,
                        price: quote.price.to_btc().to_string(),
                        min_quantity: quote.min_quantity.to_btc().to_string(),
                        max_quantity: quote.max_quantity.to_btc().to_string(),
                        refund_policy: refund_policy_label(&quote.refund_policy),
                    },
                    internal_list_sellers::Status::Unreachable => seller_data::Status {
                        offline: true,
                        price: "".to_string(),
                        min_quantity: "".to_string(),
                        max_quantity: "".to_string(),
                        refund_policy: "".to_string(),
                    },
                },
            })
            .rev()
            .collect(),
    };
    sellers_result_to_jstring(&env, &result)
}
