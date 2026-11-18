use crate::monero::TransferProof;
use crate::protocol::bob;
use crate::protocol::bob::BobState;
use monero_rpc::wallet::BlockHeight;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};
use std::fmt;

#[serde_as]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub enum Bob {
    Started {
        #[serde(with = "::bitcoin::amount::serde::as_sat")]
        btc_amount: bitcoin::Amount,
        #[serde(with = "crate::bitcoin::address_serde")]
        change_address: bitcoin::Address,
    },
    ExecutionSetupDone {
        state2: bob::State2,
    },
    BtcLockSigned {
        state3: bob::State3,
        signed_transaction: bitcoin::Transaction,
        monero_wallet_restore_blockheight: BlockHeight,
    },
    BtcLocked {
        state3: bob::State3,
        monero_wallet_restore_blockheight: BlockHeight,
    },
    XmrLockProofReceived {
        state: bob::State3,
        lock_transfer_proof: TransferProof,
        monero_wallet_restore_blockheight: BlockHeight,
    },
    XmrLocked {
        state4: bob::State4,
    },
    EncSigSent {
        state4: bob::State4,
    },
    BtcRedeemed(bob::State5),
    CancelTimelockExpired(bob::State6),
    BtcCancelled(bob::State6),
    Done(BobEndState),
}

#[derive(Clone, strum::Display, Debug, Deserialize, Serialize, PartialEq)]
pub enum BobEndState {
    SafelyAborted,
    XmrRedeemed { tx_lock_id: bitcoin::Txid },
    BtcRefunded(Box<bob::State6>),
    BtcPunished { tx_lock_id: bitcoin::Txid },
}

impl From<BobState> for Bob {
    fn from(bob_state: BobState) -> Self {
        match bob_state {
            BobState::Started {
                btc_amount,
                change_address,
            } => Bob::Started {
                btc_amount,
                change_address,
            },
            BobState::SwapSetupCompleted(state2) => Bob::ExecutionSetupDone { state2 },
            BobState::BtcLockSigned {
                state3,
                signed_transaction,
                monero_wallet_restore_blockheight,
            } => Bob::BtcLockSigned {
                state3,
                signed_transaction,
                monero_wallet_restore_blockheight,
            },
            BobState::BtcLocked {
                state3,
                monero_wallet_restore_blockheight,
            } => Bob::BtcLocked {
                state3,
                monero_wallet_restore_blockheight,
            },
            BobState::XmrLockProofReceived {
                state,
                lock_transfer_proof,
                monero_wallet_restore_blockheight,
            } => Bob::XmrLockProofReceived {
                state,
                lock_transfer_proof,
                monero_wallet_restore_blockheight,
            },
            BobState::XmrLocked(state4) => Bob::XmrLocked { state4 },
            BobState::EncSigSent(state4) => Bob::EncSigSent { state4 },
            BobState::BtcRedeemed(state5) => Bob::BtcRedeemed(state5),
            BobState::CancelTimelockExpired(state6) => Bob::CancelTimelockExpired(state6),
            BobState::BtcCancelled(state6) => Bob::BtcCancelled(state6),
            BobState::BtcRefunded(state6) => Bob::Done(BobEndState::BtcRefunded(Box::new(state6))),
            BobState::XmrRedeemed { tx_lock_id } => {
                Bob::Done(BobEndState::XmrRedeemed { tx_lock_id })
            }
            BobState::BtcPunished { tx_lock_id } => {
                Bob::Done(BobEndState::BtcPunished { tx_lock_id })
            }
            BobState::SafelyAborted => Bob::Done(BobEndState::SafelyAborted),
        }
    }
}

impl From<Bob> for BobState {
    fn from(db_state: Bob) -> Self {
        match db_state {
            Bob::Started {
                btc_amount,
                change_address,
            } => BobState::Started {
                btc_amount,
                change_address,
            },
            Bob::ExecutionSetupDone { state2 } => BobState::SwapSetupCompleted(state2),
            Bob::BtcLockSigned {
                state3,
                signed_transaction,
                monero_wallet_restore_blockheight,
            } => BobState::BtcLockSigned {
                state3,
                signed_transaction,
                monero_wallet_restore_blockheight,
            },
            Bob::BtcLocked {
                state3,
                monero_wallet_restore_blockheight,
            } => BobState::BtcLocked {
                state3,
                monero_wallet_restore_blockheight,
            },
            Bob::XmrLockProofReceived {
                state,
                lock_transfer_proof,
                monero_wallet_restore_blockheight,
            } => BobState::XmrLockProofReceived {
                state,
                lock_transfer_proof,
                monero_wallet_restore_blockheight,
            },
            Bob::XmrLocked { state4 } => BobState::XmrLocked(state4),
            Bob::EncSigSent { state4 } => BobState::EncSigSent(state4),
            Bob::BtcRedeemed(state5) => BobState::BtcRedeemed(state5),
            Bob::CancelTimelockExpired(state6) => BobState::CancelTimelockExpired(state6),
            Bob::BtcCancelled(state6) => BobState::BtcCancelled(state6),
            Bob::Done(end_state) => match end_state {
                BobEndState::SafelyAborted => BobState::SafelyAborted,
                BobEndState::XmrRedeemed { tx_lock_id } => BobState::XmrRedeemed { tx_lock_id },
                BobEndState::BtcRefunded(state6) => BobState::BtcRefunded(*state6),
                BobEndState::BtcPunished { tx_lock_id } => BobState::BtcPunished { tx_lock_id },
            },
        }
    }
}

impl fmt::Display for Bob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Bob::Started { .. } => write!(f, "Started"),
            Bob::ExecutionSetupDone { .. } => f.write_str("Execution setup done"),
            Bob::BtcLockSigned { .. } => f.write_str("Bitcoin lock transaction signed"),
            Bob::BtcLocked { .. } => f.write_str("Bitcoin locked"),
            Bob::XmrLockProofReceived { .. } => {
                f.write_str("XMR lock transaction transfer proof received")
            }
            Bob::XmrLocked { .. } => f.write_str("Monero locked"),
            Bob::CancelTimelockExpired(_) => f.write_str("Cancel timelock is expired"),
            Bob::BtcCancelled(_) => f.write_str("Bitcoin refundable"),
            Bob::BtcRedeemed(_) => f.write_str("Monero redeemable"),
            Bob::Done(end_state) => write!(f, "Done: {}", end_state),
            Bob::EncSigSent { .. } => f.write_str("Encrypted signature sent"),
        }
    }
}


/// Legacy 1.0 recovery (matrix L-01): rows written by the frozen 1.0 engine must keep loading.
/// These are the persisted JSON shapes, written out by hand; a serde change that breaks them
/// would make old swaps unrecoverable without any other signal.
#[cfg(test)]
mod legacy_row_tests {
    use super::*;

    const TXID: &str = "0d0f78dd3d66aeba552202d5034b7a015e70f1ac2b1249f0e6472230be48720e";

    fn load(json: &str) -> BobState {
        serde_json::from_str::<Bob>(json).expect("legacy row must deserialize").into()
    }

    #[test]
    fn a_safely_aborted_row_loads() {
        assert_eq!(load(r#"{"Done":"SafelyAborted"}"#), BobState::SafelyAborted);
    }

    #[test]
    fn a_redeemed_row_keeps_its_lock_txid() {
        let state = load(&format!(r#"{{"Done":{{"XmrRedeemed":{{"tx_lock_id":"{TXID}"}}}}}}"#));
        assert_eq!(
            state,
            BobState::XmrRedeemed { tx_lock_id: TXID.parse().unwrap() }
        );
    }

    #[test]
    fn a_punished_row_keeps_its_lock_txid() {
        let state = load(&format!(r#"{{"Done":{{"BtcPunished":{{"tx_lock_id":"{TXID}"}}}}}}"#));
        assert_eq!(
            state,
            BobState::BtcPunished { tx_lock_id: TXID.parse().unwrap() }
        );
    }

    #[test]
    fn a_started_row_loads_with_its_amount_and_change_address() {
        let state = load(
            r#"{"Started":{"btc_amount":99757,"change_address":"tb1qpgfn0mxwvhq3ktqxq9hfh7e8d05tx7mmw892mc"}}"#,
        );
        match state {
            BobState::Started { btc_amount, .. } => {
                assert_eq!(btc_amount, bitcoin::Amount::from_sat(99_757))
            }
            other => panic!("unexpected state {other}"),
        }
    }

    #[test]
    fn an_unknown_row_is_an_error_not_a_panic() {
        assert!(serde_json::from_str::<Bob>(r#"{"NoSuchState":1}"#).is_err());
    }
}
