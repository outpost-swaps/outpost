use crate::bitcoin::{self, ExpiredTimelocks, Txid};
use crate::protocol::alice::AliceState;
use crate::protocol::Database;
use anyhow::{bail, Result};
use std::convert::TryInto;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Cannot punish swap because it is in state {0} which is not punishable")]
    SwapNotPunishable(AliceState),
}

pub async fn punish(
    swap_id: Uuid,
    bitcoin_wallet: Arc<bitcoin::Wallet>,
    db: Arc<dyn Database>,
) -> Result<(Option<Txid>, AliceState)> {
    let state: AliceState = db.get_state(swap_id).await?.try_into()?;

    let state3 = match state {
        // Punish potentially possible (no knowledge of cancel transaction)
        AliceState::BtcLockTransactionSeen { state3 }
        | AliceState::BtcLocked { state3, .. }
        | AliceState::XmrLockTransactionSent {state3, ..}
        | AliceState::XmrLocked {state3, ..}
        | AliceState::XmrLockTransferProofSent {state3, ..}
        | AliceState::EncSigLearned {state3, ..}
        | AliceState::CancelTimelockExpired {state3, ..}
        // Punish possible due to cancel transaction already being published
        | AliceState::BtcCancelled {state3, ..}
        | AliceState::BtcPunishable {state3, ..} => { state3 }
        // The state machine is in a state where punish is theoretically impossible but we try and punish anyway as this is what the user wants
        AliceState::BtcRedeemTransactionPublished { state3 }
        | AliceState::BtcRefunded { state3,.. }
        | AliceState::Started { state3 }  => { state3 }
        // Alice already in final state: the terminal state carries no keys,
        // but the swap may still be punishable if a prior state row carries
        // them (e.g. a SafelyAborted swap whose BTC lock is funded and whose
        // punish timelock has since expired) - fall back to that row.
        AliceState::BtcRedeemed
        | AliceState::XmrRefunded
        | AliceState::BtcPunished
        | AliceState::SafelyAborted => {
            tracing::warn!(%swap_id, "Swap is already in a terminal state; recovering the swap keys from the most recent keyed state row");
            recover_latest_state3(swap_id, &db).await?
        }
    };

    tracing::info!(%swap_id, "Trying to manually punish swap");

    // Safety: refuse to broadcast unless the punish timelock has actually
    // expired - punishing earlier would destroy a still-refundable swap.
    match state3.expired_timelocks(&bitcoin_wallet).await {
        Ok(epoch) if !matches!(epoch, ExpiredTimelocks::Punish) => {
            bail!(
                "Punish timelock has not expired (current epoch: {:?}); refusing to punish because the swap may still be refundable",
                epoch
            );
        }
        Ok(_) => {}
        Err(e) => {
            // Fail closed: an RPC failure is not proof that the punish
            // timelock expired. Punishing a still-refundable swap would
            // destroy the buyer's refund path.
            bail!(
                "Could not verify punish timelock status ({}); refusing to punish until it can be verified",
                e
            );
        }
    }

    let txid = match state3.punish_btc(&bitcoin_wallet).await {
        Ok(txid) => txid,
        Err(error) => {
            // The lock output can already be spent: a previous punish
            // broadcast that never got its state persisted, or a refund that
            // raced us. Either way the funds are no longer locked and the
            // swap can never be punished again - persist the terminal state
            // so resurrection/sweeps stop retrying this swap forever.
            let error_string = format!("{:#}", error);
            if error_string.contains("already in utxo set")
                || error_string.contains("already in block chain")
                || error_string.contains("already spent")
            {
                tracing::warn!(%swap_id, "Punish broadcast found the lock output already spent; marking the swap as punished anyway so it is not resurrected repeatedly");
                let state = AliceState::BtcPunished;
                db.insert_latest_state(swap_id, state.clone().into()).await?;
                return Ok((None, state));
            }
            return Err(error);
        }
    };

    let state = AliceState::BtcPunished;
    db.insert_latest_state(swap_id, state.clone().into())
        .await?;

    Ok((Some(txid), state))
}

/// Walk the swap's persisted state history (oldest to newest) and return the
/// most recent state that still carries the swap keys. Terminal states record
/// only the outcome; the keys live in the prior keyed rows.
pub(crate) async fn recover_latest_state3<D: Database + ?Sized>(
    swap_id: Uuid,
    db: &Arc<D>,
) -> Result<Box<crate::protocol::alice::State3>> {
    let states = db.get_all_states(swap_id).await?;

    for state in states.into_iter().rev() {
        let state: AliceState = match state.try_into() {
            Ok(state) => state,
            Err(_) => continue, // a Bob-side row in a shared database
        };

        match state {
            AliceState::Started { state3 }
            | AliceState::BtcLockTransactionSeen { state3 }
            | AliceState::BtcLocked { state3 }
            | AliceState::XmrLockTransactionSent { state3, .. }
            | AliceState::XmrLocked { state3, .. }
            | AliceState::XmrLockTransferProofSent { state3, .. }
            | AliceState::EncSigLearned { state3, .. }
            | AliceState::BtcRedeemTransactionPublished { state3 }
            | AliceState::CancelTimelockExpired { state3, .. }
            | AliceState::BtcCancelled { state3, .. }
            | AliceState::BtcPunishable { state3, .. }
            | AliceState::BtcRefunded { state3, .. } => return Ok(state3),
            AliceState::BtcRedeemed
            | AliceState::XmrRefunded
            | AliceState::BtcPunished
            | AliceState::SafelyAborted => continue,
        }
    }

    bail!("No persisted state carries the swap keys; cannot punish this swap");
}
