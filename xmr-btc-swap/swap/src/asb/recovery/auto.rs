use crate::bitcoin;
use crate::protocol::{Database, State};
use anyhow::Result;
use std::sync::Arc;
use uuid::Uuid;

use super::punish::punish;

/// Sweep every swap at ASB startup (and periodically) for swaps that are
/// stuck in a terminal state while the buyer's Bitcoin is still locked and
/// only recoverable through the punish path.
///
/// This happens when the ASB aborted a swap before locking XMR (e.g. the
/// Monero funding failed after the buyer's BTC lock confirmed), the buyer then
/// cancelled, and the punish timelock expired before a refund could land.
/// The locked BTC is then spendable ONLY by the punish transaction, which the
/// aborted swap will never publish on its own.
///
/// Safety properties:
/// - Only swaps whose latest state is `SafelyAborted` are considered; live
///   swaps punish through the normal resume state machine instead.
/// - `punish()` enforces the punish timelock (fail-closed): a swap that is
///   still refundable, unfunded, or already spent is never touched.
/// - The punish transaction pays the setup-time punish address in this ASB's
///   own Bitcoin wallet.
pub async fn auto_punish_stuck_swaps(
    db: Arc<dyn Database>,
    bitcoin_wallet: Arc<bitcoin::Wallet>,
) -> Result<()> {
    for (swap_id, state) in db.all().await? {
        if !matches!(state, State::Alice(_)) {
            continue;
        }

        let alice_state: Result<crate::protocol::alice::AliceState, _> = state.try_into();
        if !matches!(alice_state, Ok(crate::protocol::alice::AliceState::SafelyAborted)) {
            continue;
        }

        match punish(swap_id, bitcoin_wallet.clone(), db.clone()).await {
            Ok((txid, _)) => match txid {
                Some(txid) => {
                    tracing::info!(%swap_id, %txid, "ASB_AUTO_PUNISH_TX");
                }
                None => {
                    tracing::info!(%swap_id, "ASB_AUTO_PUNISH_ALREADY_SPENT");
                }
            },
            Err(error) => {
                tracing::info!(%swap_id, %error, "ASB_AUTO_PUNISH_SKIPPED");
            }
        }
    }

    Ok(())
}
