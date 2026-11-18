use crate::model::get_history_request::GetHistoryRequest;
use crate::model::get_history_response::GetHistoryResponse;
use crate::model::swap_data::SwapData;
use jni::objects::JClass;
use jni::sys::jstring;
use jni::JNIEnv;
use swap::database::StateCipher;
use swap::protocol::State;
use swap::{fs, util};
use uuid::Uuid;

#[tokio::main]
#[no_mangle]
#[warn(unused_variables)]
pub async extern "system" fn Java_swap_lib_AppAsb_getHistory(
    env: JNIEnv,
    _class: JClass,
    get_history_request_json: jstring,
) -> jstring {
    let get_history_request_json_string =
        match util::get_string_value(&env, get_history_request_json) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(?error, "Failed to read ASB history request");
                return empty_history(&env);
            }
        };
    let get_history_request: GetHistoryRequest =
        match serde_json::from_str(get_history_request_json_string.as_str()) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(?error, "Failed to parse ASB history request");
                return empty_history(&env);
            }
        };

    let cipher = match get_history_request.seed_base64.as_deref() {
        Some(encoded) => match crate::util::decode_seed(encoded) {
            Ok(seed) => Some(StateCipher::from_seed(&seed)),
            Err(error) => {
                tracing::error!(?error, "Invalid seed supplied for ASB history decryption");
                return empty_history(&env);
            }
        },
        None => None,
    };

    let data_dir = match fs::system_data_dir() {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to resolve data directory for ASB history");
            return empty_history(&env);
        }
    };
    let data_dir = data_dir.join("asb");
    let data_dir = data_dir.join(if get_history_request.testnet {
        "testnet"
    } else {
        "mainnet"
    });
    let db = match swap::database::SqliteDatabase::open_with_cipher(
        data_dir.join("sqlite"),
        cipher,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to open ASB history database");
            return empty_history(&env);
        }
    };

    let swaps = match db.all_with_dates().await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(?error, "Failed to read ASB history from database");
            return empty_history(&env);
        }
    };
    #[cfg(feature = "protocol-v5")]
    let protocol_v5 = db
        .all_protocol_v5_states("alice")
        .await
        .unwrap_or_default();
    #[cfg(not(feature = "protocol-v5"))]
    let protocol_v5 = Vec::new();

    let swaps_final = build_asb_history(&swaps, protocol_v5);
    let result = GetHistoryResponse { swaps: swaps_final };
    match serde_json::to_string(&result) {
        Ok(response_json) => match env.new_string(response_json) {
            Ok(response) => response.into_inner(),
            Err(error) => {
                tracing::error!(?error, "Failed to allocate ASB history response string");
                std::ptr::null_mut()
            }
        },
        Err(error) => {
            tracing::error!(?error, "Failed to serialize ASB history response");
            empty_history(&env)
        }
    }
}

pub(crate) fn build_asb_history(
    legacy: &[(Uuid, State, String)],
    protocol_v5: Vec<(Uuid, serde_json::Value, String)>,
) -> Vec<SwapData> {
    let mut swaps_final: Vec<SwapData> = legacy
        .iter()
        .map(|(swap_id, state, date)| match state {
            State::Alice(stateAlice) => SwapData {
                swap_id: swap_id.to_string(),
                status: stateAlice.to_string(),
                date: date.clone(),
                btc_lock_txid: None,
                btc_refund_txid: None,
                xmr_lock_txid: None,
                xmr_redeem_txid: None,
            },
            State::Bob(_) => SwapData {
                swap_id: swap_id.to_string(),
                status: "INVALID_STATE".to_string(),
                date: date.clone(),
                btc_lock_txid: None,
                btc_refund_txid: None,
                xmr_lock_txid: None,
                xmr_redeem_txid: None,
            },
        })
        .rev()
        .collect();

    #[cfg(feature = "protocol-v5")]
    {
        use std::collections::HashSet;
        use swap::protocol_v5::db::alice::Alice as AliceV5;

        let existing: HashSet<String> = swaps_final
            .iter()
            .map(|swap| swap.swap_id.clone())
            .collect();

        for (swap_id, value, date) in protocol_v5 {
            if existing.contains(&swap_id.to_string()) {
                continue;
            }
            let Ok(state) = serde_json::from_value::<AliceV5>(value) else {
                tracing::warn!(%swap_id, "Skipping ASB history row with invalid protocol-5 state");
                continue;
            };
            swaps_final.push(SwapData {
                swap_id: swap_id.to_string(),
                status: state.to_string(),
                date: swap::database::format_history_date(&date),
                btc_lock_txid: None,
                btc_refund_txid: None,
                xmr_lock_txid: None,
                xmr_redeem_txid: None,
            });
        }
    }
    #[cfg(not(feature = "protocol-v5"))]
    {
        let _ = protocol_v5;
    }

    swaps_final.sort_by(|left, right| right.date.cmp(&left.date));
    swaps_final
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "protocol-v5")]
    #[test]
    fn v5_asb_history_includes_alice_state_row() {
        use swap::protocol_v5::db::alice::{Alice as AliceV5, AliceEndState};

        let swap_id = Uuid::new_v4();
        let state = AliceV5::Done(AliceEndState::SafelyAborted);
        let value = serde_json::to_value(&state).unwrap();

        let rows = build_asb_history(
            &[],
            vec![(
                swap_id,
                value,
                "2026-11-03 17:02:26.967286 +00:00:00".to_owned(),
            )],
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].swap_id, swap_id.to_string());
        assert_eq!(rows[0].status, state.to_string());
        assert_eq!(rows[0].date, "2026-11-03 17:02");
    }

    #[cfg(feature = "protocol-v5")]
    #[test]
    fn v5_asb_history_does_not_duplicate_legacy_rows() {
        use swap::protocol::alice::AliceState;
        use swap::protocol_v5::db::alice::{Alice as AliceV5, AliceEndState};

        let swap_id = Uuid::new_v4();
        let value = serde_json::to_value(AliceV5::Done(AliceEndState::SafelyAborted)).unwrap();
        let legacy = [(
            swap_id,
            State::Alice(AliceState::BtcRedeemed),
            "2026-11-03 17:00".to_owned(),
        )];

        let rows = build_asb_history(
            &legacy,
            vec![(
                swap_id,
                value,
                "2026-11-03 17:02:26.967286 +00:00:00".to_owned(),
            )],
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "btc is redeemed");
    }
}
