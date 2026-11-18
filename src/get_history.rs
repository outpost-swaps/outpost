use jni::objects::JClass;
use jni::sys::jstring;
use jni::JNIEnv;
use swap::database::StateCipher;
use swap::protocol::bob::BobState;
use swap::protocol::{Database, State};
use swap::{fs, util};

use crate::model::get_history_request::GetHistoryRequest;
use crate::model::get_history_response::GetHistoryResponse;
use crate::model::swap_data::SwapData;

#[tokio::main]
#[no_mangle]
#[warn(unused_variables)]
pub async extern "system" fn Java_swap_lib_AppSwap_getHistory(
    env: JNIEnv,
    _class: JClass,
    get_history_request_json: jstring,
) -> jstring {
    let get_history_request_json_string =
        match util::get_string_value(&env, get_history_request_json) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(?error, "Failed to read history request");
                return empty_history(&env);
            }
        };
    let get_history_request: GetHistoryRequest =
        match serde_json::from_str(get_history_request_json_string.as_str()) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(?error, "Failed to parse history request");
                return empty_history(&env);
            }
        };
    let cipher = match get_history_request.seed_base64.as_deref() {
        Some(encoded) => match crate::util::decode_seed(encoded) {
            Ok(seed) => Some(StateCipher::from_seed(&seed)),
            Err(error) => {
                tracing::error!(?error, "Invalid seed supplied for history decryption");
                return empty_history(&env);
            }
        },
        None => None,
    };

    let data_dir = match fs::system_data_dir() {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to resolve data directory for history");
            return empty_history(&env);
        }
    };
    let db = match swap::database::SqliteDatabase::open_with_cipher(
        data_dir.join("sqlite"),
        cipher,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to open history database");
            return empty_history(&env);
        }
    };

    let swaps = match db.all_with_dates().await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to read history from database");
            return empty_history(&env);
        }
    };

    // Resolve each swap's seller multiaddr independently: one orphan/partial row
    // (e.g. a crash between peer and state writes) must not panic the whole
    // history call, which would hide every other swap from auto-resume.
    for (swap_id, _state, _date) in &swaps {
        if let Err(error) = db.get_peer_id(*swap_id).await {
            tracing::warn!(swap_id = %swap_id, %error, "Skipping swap with missing peer record in history");
            continue;
        }
        // Peer record exists; addresses are resolved by resume/auto-resume paths.
    }
    #[allow(unused_mut)]
    let mut swaps_final: Vec<SwapData> = swaps
        .iter()
        .map(|(swap_id, state, date)| {
            match state {
                // A swap whose only persisted state is Started never locked
                // anything: report SAFELY_ABORTED so the GUI's auto-resume
                // filter skips it instead of re-running a doomed setup forever
                // (resuming re-runs setup from scratch, identical to a fresh
                // swap but holding the swap id hostage).
                State::Bob(BobState::Started {
                    btc_amount: _,
                    change_address: _,
                }) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "SAFELY_ABORTED".to_string(),
                    btc_lock_txid: None,
                    btc_refund_txid: None,
                    xmr_lock_txid: None,
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
                State::Bob(BobState::SwapSetupCompleted(_state2)) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "SETUP_COMPLETE".to_string(),
                    btc_lock_txid: None,
                    btc_refund_txid: None,
                    xmr_lock_txid: None,
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
                State::Bob(BobState::BtcLockSigned { state3, .. }) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "BTC_LOCK_SIGNED".to_string(),
                    btc_lock_txid: Option::from(state3.tx_lock_id().to_string()),
                    btc_refund_txid: None,
                    xmr_lock_txid: None,
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
                // Bob has locked Btc
                // Watch for Alice to Lock Xmr or for cancel timelock to elapse
                State::Bob(BobState::BtcLocked {
                    state3,
                    monero_wallet_restore_blockheight: _,
                }) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "BTC_LOCKED".to_string(),
                    btc_lock_txid: Option::from(state3.tx_lock_id().to_string()),
                    btc_refund_txid: None,
                    xmr_lock_txid: None,
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
                State::Bob(BobState::XmrLockProofReceived {
                    state,
                    lock_transfer_proof,
                    monero_wallet_restore_blockheight: _,
                }) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "XMR_LOCK_PROOF_RECEIVED".to_string(),
                    btc_lock_txid: Option::from(state.tx_lock_id().to_string()),
                    btc_refund_txid: None,
                    xmr_lock_txid: Option::from(lock_transfer_proof.tx_hash().0.to_string()),
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
                State::Bob(BobState::XmrLocked(state4)) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "XMR_LOCKED".to_string(),
                    btc_lock_txid: Option::from(state4.tx_lock.txid().to_string()),
                    btc_refund_txid: None,
                    xmr_lock_txid: None,
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
                State::Bob(BobState::EncSigSent(state)) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "ENC_SIG_SENT".to_string(),
                    btc_lock_txid: Option::from(state.tx_lock.txid().to_string()),
                    btc_refund_txid: None,
                    xmr_lock_txid: None,
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
                State::Bob(BobState::BtcRedeemed(_state5)) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "BTC_REDEEMED".to_string(),
                    btc_lock_txid: None,
                    btc_refund_txid: None,
                    xmr_lock_txid: None,
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
                State::Bob(BobState::CancelTimelockExpired(_state4)) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "CANCEL_TIMELOCK_EXPIRED".to_string(),
                    btc_lock_txid: None,
                    btc_refund_txid: None,
                    xmr_lock_txid: None,
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
                State::Bob(BobState::BtcCancelled(_state6)) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "CANCELLED".to_string(),
                    btc_lock_txid: None,
                    btc_refund_txid: None,
                    xmr_lock_txid: None,
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
                State::Bob(BobState::BtcRefunded(state6)) => {
                    // A stored state that fails to re-derive its refund transaction must not
                    // abort the batch; report the status without a refund txid instead.
                    let refund_txid = state6
                        .signed_refund_transaction()
                        .ok()
                        .map(|transaction| transaction.txid().to_string());
                    SwapData {
                        swap_id: swap_id.to_string(),
                        status: "REFUNDED".to_string(),
                        btc_lock_txid: None,
                        btc_refund_txid: refund_txid,
                        xmr_lock_txid: None,
                        xmr_redeem_txid: None,
                        date: date.clone(),
                    }
                }
                State::Bob(BobState::BtcPunished { tx_lock_id }) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "PUNISHED".to_string(),
                    btc_lock_txid: Option::from(tx_lock_id.to_string()),
                    btc_refund_txid: None,
                    xmr_lock_txid: None,
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
                State::Bob(BobState::SafelyAborted) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "SAFELY_ABORTED".to_string(),
                    btc_lock_txid: None,
                    btc_refund_txid: None,
                    xmr_lock_txid: None,
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
                State::Bob(BobState::XmrRedeemed { tx_lock_id }) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "XMR_REDEEMED".to_string(),
                    btc_lock_txid: Option::from(tx_lock_id.to_string()),
                    btc_refund_txid: None,
                    xmr_lock_txid: None,
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
                State::Alice(_) => SwapData {
                    swap_id: swap_id.to_string(),
                    status: "INVALID_STATE".to_string(),
                    btc_lock_txid: None,
                    btc_refund_txid: None,
                    xmr_lock_txid: None,
                    xmr_redeem_txid: None,
                    date: date.clone(),
                },
            }
        })
        .rev()
        .collect();

    #[cfg(feature = "protocol-v5")]
    {
        use swap::protocol_v5::db::bob::Bob as BobV5;

        if let Ok(protocol_v5) = db.all_protocol_v5_states("bob").await {
            let existing: std::collections::HashSet<String> = swaps_final
                .iter()
                .map(|swap| swap.swap_id.clone())
                .collect();

            for (swap_id, value, date) in protocol_v5 {
                if existing.contains(&swap_id.to_string()) {
                    continue;
                }
                let Ok(state) = serde_json::from_value::<BobV5>(value) else {
                    continue;
                };
                let (status, btc_lock_txid, xmr_lock_txid) = match state {
                    BobV5::Started { .. } => ("SAFELY_ABORTED".to_string(), None, None),
                    BobV5::ExecutionSetupDone { .. } => ("SETUP_COMPLETE".to_string(), None, None),
                    BobV5::BtcLockReadyToPublish { state3, .. }
                    | BobV5::BtcLocked { state3, .. } => (
                        "BTC_LOCKED".to_string(),
                        Some(state3.tx_lock_id().to_string()),
                        None,
                    ),
                    BobV5::XmrLockProofReceived {
                        state,
                        lock_transfer_proof,
                        ..
                    }
                    | BobV5::XmrLockTransactionSeen {
                        state,
                        lock_transfer_proof,
                        ..
                    } => (
                        "XMR_LOCK_PROOF_RECEIVED".to_string(),
                        Some(state.tx_lock_id().to_string()),
                        Some(lock_transfer_proof.tx_hash().0.to_string()),
                    ),
                    BobV5::XmrLocked { .. } => ("XMR_LOCKED".to_string(), None, None),
                    BobV5::EncSigReadyToBeSent { .. } | BobV5::EncSigSent { .. } => {
                        ("ENC_SIG_SENT".to_string(), None, None)
                    }
                    BobV5::BtcRedeemed(_)
                    | BobV5::XmrRedeemConstructed { .. }
                    | BobV5::XmrRedeemPublished { .. } => {
                        ("BTC_REDEEMED".to_string(), None, None)
                    }
                    BobV5::BtcPunished { .. } => ("PUNISHED".to_string(), None, None),
                    BobV5::WaitingForCancelTimelockExpiration { .. }
                    | BobV5::CancelTimelockExpired(_)
                    | BobV5::BtcCancelled(_) => {
                        ("REFUND_PENDING".to_string(), None, None)
                    }
                    BobV5::BtcCancelPublished(_) => {
                        ("REFUND_PENDING".to_string(), None, None)
                    }
                    // Refund on its way, including the wait to claim a partial refund's
                    // deposit back: still running, resumable.
                    BobV5::BtcRefundPublished(_)
                    | BobV5::BtcEarlyRefundPublished(_)
                    | BobV5::BtcPartialRefundPublished(_)
                    | BobV5::BtcPartiallyRefunded(_)
                    | BobV5::WaitingForReclaimTimelockExpiration { .. }
                    | BobV5::ReclaimTimelockExpired(_)
                    | BobV5::BtcReclaimPublished(_)
                    | BobV5::BtcWithholdPublished(_)
                    | BobV5::BtcMercyPublished(_) => {
                        ("REFUND_PENDING".to_string(), None, None)
                    }
                    // The seller burnt the deposit: the rest was refunded (audit H-1).
                    BobV5::BtcWithheld(_) => ("PARTIALLY_REFUNDED".to_string(), None, None),
                    BobV5::Done(end) => {
                        use swap::protocol_v5::db::bob::BobEndState;
                        let status = match end {
                            BobEndState::SafelyAborted => "SAFELY_ABORTED",
                            BobEndState::XmrRedeemed { .. } => "XMR_REDEEMED",
                            BobEndState::BtcRefunded(_)
                            | BobEndState::BtcEarlyRefunded(_)
                            | BobEndState::BtcReclaimConfirmed(_)
                            | BobEndState::BtcMercyConfirmed(_) => "REFUNDED",
                        };
                        (status.to_string(), None, None)
                    }
                };
                swaps_final.push(SwapData {
                    swap_id: swap_id.to_string(),
                    status,
                    date: swap::database::format_history_date(&date),
                    btc_lock_txid,
                    btc_refund_txid: None,
                    xmr_lock_txid,
                    xmr_redeem_txid: None,
                });
            }
        }
    }

    swaps_final.sort_by(|left, right| right.date.cmp(&left.date));

    let result = GetHistoryResponse { swaps: swaps_final };
    match serde_json::to_string(&result) {
        Ok(response_json) => match env.new_string(response_json) {
            Ok(response) => response.into_inner(),
            Err(error) => {
                tracing::error!(?error, "Failed to allocate history response string");
                std::ptr::null_mut()
            }
        },
        Err(error) => {
            tracing::error!(?error, "Failed to serialize history response");
            empty_history(&env)
        }
    }
}

/// A valid response for the Java side when history cannot be read. The error is
/// logged; crashing the whole app on a locked or corrupt database would hide
/// every other feature.
fn empty_history(env: &JNIEnv<'_>) -> jstring {
    match env.new_string(r#"{"swaps":[]}"#) {
        Ok(response) => response.into_inner(),
        Err(_) => std::ptr::null_mut(),
    }
}
