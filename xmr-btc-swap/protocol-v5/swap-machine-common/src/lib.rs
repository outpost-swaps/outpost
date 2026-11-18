//! Eigenwallet 5.0.2 setup-message and fee-validation subset.
//!
//! Imported from `eigen-core` commit `cc1de7f3690b29605e14dd45b897347881050f2f`.
//! The role state enum and database trait that follow the fee helpers in the
//! upstream `swap-machine/src/common/mod.rs` are intentionally not imported;
//! they require the Monero C++ wallet stack and are outside this conformance slice.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigma_fun::HashTranscript;
use sigma_fun::ext::dl_secp256k1_ed25519_eq::{CrossCurveDLEQ, CrossCurveDLEQProof};
use std::sync::LazyLock;
use swap_core::bitcoin;
use swap_core::monero;
use uuid::Uuid;

/// BIP-341 NUMS point `H = lift_x(SHA256(uncompressed_encoding(G)))`: <https://github.com/bitcoin/bips/blob/master/bip-0341.mediawiki#constructing-and-spending-taproot-outputs>
static PEDERSEN_BLINDING_H_SECP256K1: LazyLock<ecdsa_fun::fun::Point> = LazyLock::new(|| {
    let generator = (*ecdsa_fun::fun::G).normalize().to_bytes_uncompressed();
    let x_coordinate = Sha256::digest(generator).into();

    ecdsa_fun::fun::Point::<ecdsa_fun::fun::marker::EvenY>::from_xonly_bytes(x_coordinate)
        .expect("SHA-256 of the uncompressed secp256k1 generator is a valid x-coordinate")
        .normalize()
});

fn pedersen_blinding_h_ed25519() -> curve25519_dalek_ng::edwards::EdwardsPoint {
    curve25519_dalek_ng::edwards::CompressedEdwardsY::from_slice(
        &monero_oxide_wallet::ed25519::CompressedPoint::H.to_bytes(),
    )
    .decompress()
    .expect("Monero Pedersen H is a valid ed25519 point")
}

pub static CROSS_CURVE_PROOF_SYSTEM: LazyLock<
    CrossCurveDLEQ<HashTranscript<Sha256, rand_chacha::ChaCha20Rng>>,
> = LazyLock::new(|| {
    CrossCurveDLEQ::<HashTranscript<Sha256, rand_chacha::ChaCha20Rng>>::new(
        *PEDERSEN_BLINDING_H_SECP256K1,
        pedersen_blinding_h_ed25519(),
    )
});

#[allow(non_snake_case)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message0 {
    pub swap_id: Uuid,
    pub B: bitcoin::PublicKey,
    pub S_b_monero: monero_oxide_ext::PublicKey,
    pub S_b_bitcoin: bitcoin::PublicKey,
    pub dleq_proof_s_b: CrossCurveDLEQProof,
    pub v_b: monero::PrivateViewKey,
    #[serde(with = "swap_serde::bitcoin::address_serde")]
    pub refund_address: bitcoin::Address,
}

#[allow(non_snake_case)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message1 {
    pub A: bitcoin::PublicKey,
    pub S_a_monero: monero::PublicKey,
    pub S_a_bitcoin: bitcoin::PublicKey,
    pub dleq_proof_s_a: CrossCurveDLEQProof,
    pub v_a: monero::PrivateViewKey,
    #[serde(with = "swap_serde::bitcoin::address_serde")]
    pub redeem_address: bitcoin::Address,
    #[serde(with = "swap_serde::bitcoin::address_serde")]
    pub punish_address: bitcoin::Address,
    pub fee_rate: bitcoin::SwapFeeRate,
    pub redeem_multiplier: bitcoin::RedeemFeeMultiplier,
    /// The amount of Bitcoin that Bob not get refunded unless Alice decides so.
    /// Introduced in [#675](https://github.com/eigenwallet/core/pull/675) to combat spam.
    pub amnesty_amount: bitcoin::Amount,
}

#[allow(non_snake_case)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message2 {
    pub tx_lock_psbt: bitcoin::PartiallySignedTransaction,
    /// One proof per TxLock input that Bob controls the keys behind it.
    pub tx_lock_proofs: Vec<utxo_ownership_proof::OwnershipProof>,
}

/// Binds a TxLock ownership proof to the swap session and the exact lock transaction.
#[allow(non_snake_case)]
pub fn tx_lock_ownership_challenge(
    swap_id: Uuid,
    A: bitcoin::PublicKey,
    B: bitcoin::PublicKey,
    tx_lock_txid: bitcoin::Txid,
) -> utxo_ownership_proof::Challenge {
    let mut context = Vec::with_capacity(16 + 33 + 33 + 32);
    context.extend_from_slice(swap_id.as_bytes());
    context.extend_from_slice(&ecdsa_fun::fun::Point::from(A).to_bytes());
    context.extend_from_slice(&ecdsa_fun::fun::Point::from(B).to_bytes());
    context.extend_from_slice(tx_lock_txid.as_ref());
    utxo_ownership_proof::challenge(&context)
}

#[allow(non_snake_case)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message3 {
    pub tx_cancel_sig: bitcoin::Signature,
    // The following fields were reworked in [#675](https://github.com/eigenwallet/core/pull/675).
    // Alice will send either the full refund encsig or signatures for both partial refund
    // and tx refund amnesty.
    pub tx_full_refund_encsig: Option<bitcoin::EncryptedSignature>,
    pub tx_partial_refund_encsig: Option<bitcoin::EncryptedSignature>,
    pub tx_reclaim_sig: Option<bitcoin::Signature>,
}

#[allow(non_snake_case)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message4 {
    pub tx_punish_sig: bitcoin::Signature,
    pub tx_cancel_sig: bitcoin::Signature,
    pub tx_early_refund_sig: bitcoin::Signature,
    pub tx_reclaim_sig: Option<bitcoin::Signature>,
    pub tx_withhold_sig: Option<bitcoin::Signature>,
    pub tx_mercy_sig: Option<bitcoin::Signature>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SetupValidationError {
    #[error("Invalid DLEQ proof")]
    InvalidDleqProof,
    #[error("Anti-spam deposit ratio {ratio} exceeds maximum {max}")]
    AntiSpamDepositRatioTooHigh {
        ratio: rust_decimal::Decimal,
        max: rust_decimal::Decimal,
    },
    #[error("Transaction fee is too low compared to our estimate ({fee} vs {estimate})")]
    TransactionFeeTooLow {
        fee: bitcoin::Amount,
        estimate: bitcoin::Amount,
    },
}

/// Require at least half our local fee estimate for a transaction of
/// `signed_weight`, so the floor holds for the fee rate, not just the absolute
/// fee. Higher fees are allowed.
///
/// The estimate is scaled by vsize directly. It deliberately does not use
/// `bitcoin_wallet::estimate_fee`, whose flat 1,000-sat absolute minimum
/// would regress this to the old flat-floor overpay on small transactions.
///
/// `estimated_rate` must already include the relay minimum.
pub fn sanity_check_transaction_fee_floor(
    proposed_fee: bitcoin::Amount,
    signed_weight: ::bitcoin::Weight,
    estimated_rate: ::bitcoin::FeeRate,
    transfer_amount: bitcoin::Amount,
) -> Result<()> {
    use rust_decimal::prelude::ToPrimitive;

    let estimate = estimated_rate
        .checked_mul_by_weight(signed_weight)
        .context("Failed to estimate transaction fee")?;
    let relative_cap = bitcoin::Amount::from_sat(
        bitcoin_wallet::MAX_RELATIVE_TX_FEE
            .saturating_mul(rust_decimal::Decimal::from(transfer_amount.to_sat()))
            .ceil()
            .to_u64()
            .context("Relative fee cap overflow")?,
    );
    let estimate = estimate
        .min(relative_cap)
        .min(bitcoin_wallet::MAX_ABSOLUTE_TX_FEE);
    let floor = bitcoin::Amount::from_sat(estimate.to_sat() / 2);
    anyhow::ensure!(
        proposed_fee >= floor,
        SetupValidationError::TransactionFeeTooLow {
            fee: proposed_fee,
            estimate,
        }
    );
    Ok(())
}

/// Local acceptance limits, derived without capping the agreed fee schedule.
/// The lower limit is half our estimate, but never below [`bitcoin::SwapFeeRate::MIN`].
pub fn swap_fee_limits(
    rate: bitcoin::SwapFeeRate,
    amount: bitcoin::Amount,
) -> Result<bitcoin::SwapFeeLimits> {
    use anyhow::Context;
    anyhow::ensure!(rate.0 > 0, "Local fee estimate must be positive");
    Ok(bitcoin::SwapFeeLimits {
        min_rate: bitcoin::SwapFeeRate((rate.0 / 2 + rate.0 % 2).max(bitcoin::SwapFeeRate::MIN.0)),
        max_rate: bitcoin::SwapFeeRate(rate.0.checked_mul(3).context("Fee rate limit overflow")?),
        max_path_fee: bitcoin_wallet::MAX_ABSOLUTE_TX_FEE
            .min(bitcoin::Amount::from_sat(amount.to_sat() / 3)),
    })
}

pub fn calculate_swap_fees(
    inputs: bitcoin::SwapFeeInputs<'_>,
    rate: bitcoin::SwapFeeRate,
    limits: bitcoin::SwapFeeLimits,
) -> Result<bitcoin::SwapFees> {
    let ratio = rust_decimal::Decimal::from(inputs.deposit.to_sat())
        .checked_div(rust_decimal::Decimal::from(inputs.lock_amount.to_sat()))
        .context("Cannot calculate anti-spam deposit ratio for a zero lock amount")?;
    anyhow::ensure!(
        ratio <= swap_env::config::MAX_ANTI_SPAM_DEPOSIT_RATIO,
        SetupValidationError::AntiSpamDepositRatioTooHigh {
            ratio,
            max: swap_env::config::MAX_ANTI_SPAM_DEPOSIT_RATIO,
        }
    );
    inputs.calculate(rate, limits)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This crate mirrors `swap-machine`'s `common` module for the conformance
    /// vectors. The fee-acceptance functions must stay byte-identical, or the
    /// vectors would test code the app does not run (matrix S-01).
    #[test]
    fn fee_functions_match_the_ones_swap_machine_ships() {
        fn body<'a>(source: &'a str, name: &str) -> &'a str {
            let start = source
                .find(&format!("pub fn {name}"))
                .unwrap_or_else(|| panic!("{name} not found"));
            let rest = &source[start..];
            let end = rest.find("\n}\n").expect("function end") + 3;
            &rest[..end]
        }
        let shipped = include_str!("../../swap-machine/src/common/mod.rs");
        let mirror = include_str!("lib.rs");
        for name in [
            "sanity_check_transaction_fee_floor",
            "swap_fee_limits",
            "calculate_swap_fees",
        ] {
            assert_eq!(
                body(shipped, name),
                body(mirror, name),
                "{name} differs between swap-machine and swap-machine-common"
            );
        }
    }

    #[test]
    fn lock_fee_floor_is_scaled_by_vsize_not_the_wallet_absolute_minimum() {
        let one_input = ::bitcoin::Weight::from_wu(488);
        let one_sat_per_vbyte = ::bitcoin::FeeRate::from_sat_per_vb(1).unwrap();

        let error = sanity_check_transaction_fee_floor(
            bitcoin::Amount::from_sat(60),
            one_input,
            one_sat_per_vbyte,
            bitcoin::Amount::ONE_BTC,
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<SetupValidationError>(),
            Some(&SetupValidationError::TransactionFeeTooLow {
                fee: bitcoin::Amount::from_sat(60),
                estimate: bitcoin::Amount::from_sat(122),
            })
        );
        assert!(
            sanity_check_transaction_fee_floor(
                bitcoin::Amount::from_sat(61),
                one_input,
                one_sat_per_vbyte,
                bitcoin::Amount::ONE_BTC,
            )
            .is_ok()
        );

        let max_inputs = ::bitcoin::Weight::from_wu(7_016);
        assert!(
            sanity_check_transaction_fee_floor(
                bitcoin::Amount::from_sat(876),
                max_inputs,
                one_sat_per_vbyte,
                bitcoin::Amount::ONE_BTC,
            )
            .is_err()
        );
        assert!(
            sanity_check_transaction_fee_floor(
                bitcoin::Amount::from_sat(877),
                max_inputs,
                one_sat_per_vbyte,
                bitcoin::Amount::ONE_BTC,
            )
            .is_ok()
        );
    }
}
