//! Cross-client conformance with UNMODIFIED Eigenwallet 5.0.2 (matrix S-01, WORKLOG §461).
//!
//! Both sides derive every presigned fee from the maker's advertised rate with code compiled
//! into each client; nothing is negotiated, so a difference only shows as a failed signature
//! check against a real Eigenwallet peer (WORKLOG §343). The vectors in
//! `eigenwallet-5.0.2/fee-vectors.json` were computed by Eigenwallet's own code; our code must
//! produce exactly the same values.
use swap_core::bitcoin::{self, RedeemFeeMultiplier, SwapFeeInputs, SwapFeeRate};

const VECTORS: &str = include_str!("../eigenwallet-5.0.2/fee-vectors.json");

fn vectors() -> serde_json::Value {
    serde_json::from_str(VECTORS).unwrap()
}

fn key(byte: u8) -> bitcoin::PublicKey {
    let scalar = ecdsa_fun::fun::Scalar::from_bytes_mod_order([byte; 32])
        .non_zero()
        .unwrap();
    bitcoin::PublicKey::from(scalar)
}

fn u(value: &serde_json::Value) -> u64 {
    value.as_u64().unwrap()
}

#[test]
fn the_vectors_are_from_eigenwallet_5_0_2() {
    let v = vectors();
    assert!(v["source"].as_str().unwrap().contains("cc1de7f3690b29605e14dd45b897347881050f2f"));
    assert_eq!(u(&v["tx_lock_weight"]), bitcoin::TxLock::weight().to_wu());
    assert_eq!(v["fees"].as_array().unwrap().len(), 112);
}

/// Every presigned fee (base, redeem, the measured template size) matches, and the same
/// schedules are refused.
#[test]
fn our_fee_schedule_matches_eigenwallet() {
    let address = "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx"
        .parse::<::bitcoin::Address<::bitcoin::address::NetworkUnchecked>>()
        .unwrap()
        .assume_checked();
    for case in vectors()["fees"].as_array().unwrap() {
        let (rate, amount) = (u(&case["rate"]), u(&case["amount"]));
        let inputs = SwapFeeInputs {
            a: key(7),
            b: key(9),
            redeem_address: &address,
            refund_address: &address,
            punish_address: &address,
            lock_amount: ::bitcoin::Amount::from_sat(amount),
            deposit: ::bitcoin::Amount::from_sat(amount * u(&case["deposit_percent"]) / 100),
            redeem_multiplier: RedeemFeeMultiplier::try_from(u(&case["redeem_multiplier"]) as u32).unwrap(),
        };
        let limits = swap_machine_common::swap_fee_limits(SwapFeeRate(rate), ::bitcoin::Amount::from_sat(amount)).unwrap();
        let ours = inputs.calculate(SwapFeeRate(rate), limits);
        let theirs = &case["result"];
        match ours {
            Ok(fees) => {
                assert!(theirs.get("error").is_none(), "we accept, Eigenwallet refuses: {case}");
                assert_eq!(fees.base.to_sat(), u(&theirs["base"]), "base fee differs: {case}");
                assert_eq!(fees.redeem.to_sat(), u(&theirs["redeem"]), "redeem fee differs: {case}");
                assert_eq!(fees.max_vsize, u(&theirs["max_vsize"]), "template size differs: {case}");
            }
            Err(error) => assert!(theirs.get("error").is_some(), "we refuse ({error}), Eigenwallet accepts: {case}"),
        }
    }
}

/// The rate window a buyer accepts (half to 3x its estimate, and the path budget).
#[test]
fn our_fee_limits_match_eigenwallet() {
    for case in vectors()["limits"].as_array().unwrap() {
        let limits = swap_machine_common::swap_fee_limits(
            SwapFeeRate(u(&case["rate"])),
            ::bitcoin::Amount::from_sat(u(&case["amount"])),
        )
        .unwrap();
        assert_eq!(limits.min_rate.0, u(&case["min_rate"]), "{case}");
        assert_eq!(limits.max_rate.0, u(&case["max_rate"]), "{case}");
        assert_eq!(limits.max_path_fee.to_sat(), u(&case["max_path_fee"]), "{case}");
    }
}
