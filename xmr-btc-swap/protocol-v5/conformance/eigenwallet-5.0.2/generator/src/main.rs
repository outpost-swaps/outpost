//! Golden fee vectors from UNMODIFIED Eigenwallet 5.0.2 (commit cc1de7f3), for Outpost's
//! cross-client conformance test (matrix S-01). Run inside the Eigenwallet workspace:
//!   cargo run -p conformance-vectors > eigenwallet-5.0.2-fee-vectors.json
//! The fee schedule comes from their swap-core directly. `swap_fee_limits` and the lock-floor
//! estimate are copied from their swap-machine/src/common/mod.rs (which needs monero-sys to
//! build); `copied_code_is_verbatim` below fails if the copy drifts from their file.
use anyhow::Result;
use std::str::FromStr;
use swap_core::bitcoin;

// ---- copied verbatim from swap-machine/src/common/mod.rs (5.0.2) ----
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
// ---- end of copy ----

/// The maker's lock floor, as `sanity_check_transaction_fee_floor` computes it: half of
/// `bitcoin_wallet::estimate_fee(weight, amount, rate, rate)`.
fn lock_floor(rate_sat_per_kvb: u64, amount: u64) -> Result<u64> {
    let rate = ::bitcoin::FeeRate::from_sat_per_kwu(rate_sat_per_kvb / 4);
    let estimate = bitcoin_wallet::estimate_fee(
        bitcoin::TxLock::weight(),
        Some(::bitcoin::Amount::from_sat(amount)),
        rate,
        rate,
    )?;
    Ok(estimate.to_sat() / 2)
}

fn key(byte: u8) -> bitcoin::PublicKey {
    let scalar = ecdsa_fun::fun::Scalar::from_bytes_mod_order([byte; 32]).non_zero().expect("non-zero");
    bitcoin::PublicKey::from(scalar)
}

fn main() -> Result<()> {
    let source = include_str!("../../swap-machine/src/common/mod.rs");
    let copy = include_str!("main.rs");
    let start = copy.find("/// Local acceptance limits").unwrap();
    let end = copy.find("// ---- end of copy ----").unwrap();
    assert!(source.contains(copy[start..end].trim_end()), "swap_fee_limits copy drifted from 5.0.2");

    let address = ::bitcoin::Address::from_str("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx")?.assume_checked();
    let rates = [1_000u64, 2_000, 3_000, 6_132, 10_000, 25_000, 50_000];
    let amounts = [100_000u64, 809_500, 1_500_000, 10_000_000];
    let mut fees = Vec::new();
    for &rate in &rates {
        for &amount in &amounts {
            for &deposit_percent in &[0u64, 10] {
                for &multiplier in &[200u32, 250] {
                    let inputs = bitcoin::SwapFeeInputs {
                        a: key(7),
                        b: key(9),
                        redeem_address: &address,
                        refund_address: &address,
                        punish_address: &address,
                        lock_amount: ::bitcoin::Amount::from_sat(amount),
                        deposit: ::bitcoin::Amount::from_sat(amount * deposit_percent / 100),
                        redeem_multiplier: bitcoin::RedeemFeeMultiplier::try_from(multiplier)?,
                    };
                    let limits = swap_fee_limits(bitcoin::SwapFeeRate(rate), ::bitcoin::Amount::from_sat(amount))?;
                    let result = match inputs.calculate(bitcoin::SwapFeeRate(rate), limits) {
                        Ok(f) => serde_json::json!({"max_vsize": f.max_vsize, "base": f.base.to_sat(), "redeem": f.redeem.to_sat()}),
                        Err(e) => serde_json::json!({"error": e.to_string()}),
                    };
                    fees.push(serde_json::json!({
                        "rate": rate, "amount": amount, "deposit_percent": deposit_percent,
                        "redeem_multiplier": multiplier, "result": result,
                    }));
                }
            }
        }
    }
    let mut limits = Vec::new();
    let mut floors = Vec::new();
    for &rate in &rates {
        for &amount in &amounts {
            let l = swap_fee_limits(bitcoin::SwapFeeRate(rate), ::bitcoin::Amount::from_sat(amount))?;
            limits.push(serde_json::json!({"rate": rate, "amount": amount,
                "min_rate": l.min_rate.0, "max_rate": l.max_rate.0, "max_path_fee": l.max_path_fee.to_sat()}));
            floors.push(serde_json::json!({"rate": rate, "amount": amount, "lock_floor": lock_floor(rate, amount)?}));
        }
    }
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
        "source": "eigenwallet/core cc1de7f3690b29605e14dd45b897347881050f2f (5.0.2)",
        "keys": "a = scalar [7; 32], b = scalar [9; 32]",
        "address": "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx (redeem, refund, punish)",
        "tx_lock_weight": bitcoin::TxLock::weight().to_wu(),
        "fees": fees, "limits": limits, "lock_floors": floors,
    }))?);
    Ok(())
}
