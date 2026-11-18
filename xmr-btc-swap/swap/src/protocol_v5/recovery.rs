use crate::monero;
use crate::protocol_v5::core::monero as v5_monero;
use crate::protocol_v5::db;
use anyhow::{bail, ensure, Context, Result};

/// XMR keys recovered from a self-ASB swap where Alice punished Bob.
///
/// The caller must still verify the transfer proof against the Monero chain and
/// publish the sweep explicitly; this module never broadcasts.
pub struct RecoveredXmrKeys {
    pub spend_key: monero::PrivateKey,
    pub view_key: monero::PrivateViewKey,
    pub amount_pico: u64,
    pub public_spend_key: v5_monero::PublicKey,
}

pub struct RecoveredXmr {
    pub keys: RecoveredXmrKeys,
    pub txid: monero::TxHash,
}

/// Recover the combined Monero key from Alice's `BtcPunished` state and Bob's
/// cancelled/punished state. Both states must come from the same seed.
pub fn recover_xmr_from_punished_self_swap(
    bob: &db::bob::Bob,
    alice: &db::alice::Alice,
) -> Result<RecoveredXmr> {
    let state6 = match bob {
        db::bob::Bob::CancelTimelockExpired(state6)
        | db::bob::Bob::BtcCancelPublished(state6)
        | db::bob::Bob::BtcCancelled(state6)
        | db::bob::Bob::BtcRefundPublished(state6)
        | db::bob::Bob::BtcEarlyRefundPublished(state6)
        | db::bob::Bob::BtcPartialRefundPublished(state6)
        | db::bob::Bob::BtcPartiallyRefunded(state6)
        | db::bob::Bob::WaitingForReclaimTimelockExpiration(state6)
        | db::bob::Bob::ReclaimTimelockExpired(state6)
        | db::bob::Bob::BtcReclaimPublished(state6)
        | db::bob::Bob::BtcWithholdPublished(state6)
        | db::bob::Bob::BtcWithheld(state6)
        | db::bob::Bob::BtcMercyPublished(state6)
        | db::bob::Bob::BtcPunished { state: state6, .. } => state6,
        other => bail!("Bob state does not retain the cancelled swap's Monero key share: {other}"),
    };

    let (state3, transfer_proof) = match alice {
        db::alice::Alice::Done(db::alice::AliceEndState::BtcPunished {
            state3,
            transfer_proof,
        }) => (state3, transfer_proof),
        db::alice::Alice::BtcPunishable {
            state3,
            transfer_proof,
            ..
        } => (state3, transfer_proof),
        other => bail!("Alice state has no punished Monero lock: {other}"),
    };

    let transfer_proof = transfer_proof
        .as_ref()
        .context("Alice state is missing the Monero transfer proof")?;

    let keys = combine_recovery_keys(
        state3.s_a,
        state6.recovery_spend_key_share(),
        state6
            .recovery_public_spend_key_share()
            .context("Bob state is missing Alice's public Monero share")?,
        state3.S_b_monero,
        &state3.v,
        &state6.recovery_view_key(),
        state3.xmr.as_pico(),
        state6.recovery_amount_pico(),
    )?;

    Ok(RecoveredXmr {
        keys,
        txid: monero::TxHash(transfer_proof.tx_hash.0.clone()),
    })
}

pub fn combine_recovery_keys(
    s_a: v5_monero::Scalar,
    s_b: v5_monero::Scalar,
    s_a_public: v5_monero::PublicKey,
    s_b_public: v5_monero::PublicKey,
    view_a: &v5_monero::PrivateViewKey,
    view_b: &v5_monero::PrivateViewKey,
    amount_a: u64,
    amount_b: u64,
) -> Result<RecoveredXmrKeys> {
    let alice_share = v5_monero::PrivateKey::from_scalar(s_a);
    let bob_share = v5_monero::PrivateKey::from_scalar(s_b);

    ensure!(
        v5_monero::PublicKey::from_private_key(&alice_share) == s_a_public,
        "Alice's public Monero share does not match her private share"
    );
    ensure!(
        v5_monero::PublicKey::from_private_key(&bob_share) == s_b_public,
        "Bob's public Monero share does not match his private share"
    );
    ensure!(
        view_a.0.as_bytes() == view_b.0.as_bytes(),
        "Alice and Bob view keys do not match"
    );
    ensure!(
        amount_a == amount_b,
        "Alice and Bob state amounts do not match"
    );

    let combined = alice_share + bob_share;
    let public_spend_key = s_a_public + s_b_public;
    ensure!(
        v5_monero::PublicKey::from_private_key(&combined) == public_spend_key,
        "Combined Monero spend key does not match the shared public key"
    );

    let spend_key = monero::PrivateKey::from_slice(&combined.as_bytes())
        .context("Failed to convert the combined Monero spend key")?;
    let view_key = monero::PrivateViewKey::from_private_key(
        monero::PrivateKey::from_slice(&view_a.0.as_bytes())
            .context("Failed to convert the Monero view key")?,
    );

    Ok(RecoveredXmrKeys {
        spend_key,
        view_key,
        amount_pico: amount_a,
        public_spend_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol_v5::core::monero::{PrivateKey, PrivateViewKey, PublicKey, Scalar};
    use rand::thread_rng;

    fn shares() -> (
        Scalar,
        Scalar,
        PublicKey,
        PublicKey,
        PrivateViewKey,
        PrivateViewKey,
    ) {
        let mut rng = thread_rng();
        let s_a = Scalar::random(&mut rng);
        let s_b = Scalar::random(&mut rng);
        let s_a_public = PublicKey::from_private_key(&PrivateKey::from_scalar(s_a));
        let s_b_public = PublicKey::from_private_key(&PrivateKey::from_scalar(s_b));
        let view_a = PrivateViewKey::new_random(&mut rng);
        let view_b = PrivateViewKey::new_random(&mut rng);
        (s_a, s_b, s_a_public, s_b_public, view_a, view_b)
    }

    #[test]
    fn combines_matching_self_swap_shares() {
        let (s_a, s_b, s_a_public, s_b_public, view_a, _view_b) = shares();
        let keys =
            combine_recovery_keys(s_a, s_b, s_a_public, s_b_public, &view_a, &view_a, 123, 123)
                .unwrap();

        assert_eq!(keys.amount_pico, 123);
        assert_eq!(keys.public_spend_key, s_a_public + s_b_public);
        assert_eq!(keys.view_key.as_bytes(), view_a.0.as_bytes().as_slice());
    }

    #[test]
    fn rejects_wrong_public_share() {
        let (s_a, s_b, _s_a_public, s_b_public, view_a, _view_b) = shares();
        assert!(
            combine_recovery_keys(s_a, s_b, s_b_public, s_b_public, &view_a, &view_a, 1, 1)
                .is_err()
        );
    }

    #[test]
    fn rejects_mismatched_view_key() {
        let (s_a, s_b, s_a_public, s_b_public, view_a, view_b) = shares();
        assert!(
            combine_recovery_keys(s_a, s_b, s_a_public, s_b_public, &view_a, &view_b, 1, 1)
                .is_err()
        );
    }

    #[test]
    fn rejects_mismatched_amount() {
        let (s_a, s_b, s_a_public, s_b_public, view_a, _view_b) = shares();
        assert!(
            combine_recovery_keys(s_a, s_b, s_a_public, s_b_public, &view_a, &view_a, 1, 2)
                .is_err()
        );
    }
}
