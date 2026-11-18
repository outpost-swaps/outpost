//! A seller's swap fee rate above our limit (matrix S-01, WORKLOG §458).
//!
//! The buyer refuses a seller whose advertised fee rate (Message1) is more than 3x our own
//! Dojo estimate: the fee-griefing defense kept in §258. A seller with a cautious estimator can
//! trip it on a quiet chain (hermes-v5 on testnet asked 6.1 sat/vB while 1-2 sat/vB confirmed
//! in the next block). The refusal stays the default; the GUI shows the numbers and the user may
//! accept that seller's rate for one retry, which raises only the upper limit, for that swap.

use std::convert::TryFrom;

use jni::objects::JValue;
use jni::JNIEnv;
use swap::protocol_v5::core::bitcoin::{SwapFeeLimits, SwapFeeRate};

pub(crate) const ON_SELLER_FEE_RATE_TOO_HIGH_METHOD: &str = "onSellerFeeRateTooHigh";
pub(crate) const ON_SELLER_FEE_RATE_TOO_HIGH_SIGNATURE: &str = "([BJJJ)V";

/// The numbers of a "rate above our limit" refusal, in sat/kvB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RateAboveLimit {
    pub seller_rate: u64,
    pub our_max: u64,
}

/// Reads our own refusal message (`SwapFeeError::RateOutsideLimits`). Only a rate ABOVE the
/// limit is offered for acceptance; a rate below our minimum is never overridden.
pub(crate) fn rate_above_limit(message: &str) -> Option<RateAboveLimit> {
    let rest = message.split("Proposed swap fee rate ").nth(1)?;
    let (rate, rest) = rest.split_once(" sat/kvB is outside local limits ")?;
    let (_min, rest) = rest.split_once("..=")?;
    let (max, _) = rest.split_once(" sat/kvB")?;
    let seller_rate: u64 = rate.trim().parse().ok()?;
    let our_max: u64 = max.trim().parse().ok()?;
    (seller_rate > our_max).then_some(RateAboveLimit { seller_rate, our_max })
}

/// Headroom over the rate the seller asked, so a small move between the two attempts still passes.
pub(crate) const ACCEPTED_RATE_MARGIN_PERCENT: u64 = 110;

/// The upper limit for a seller rate the user accepted: the asked rate plus the margin.
pub(crate) fn accepted_limit(asked_sat_per_kvb: u64) -> u64 {
    (asked_sat_per_kvb * ACCEPTED_RATE_MARGIN_PERCENT).div_ceil(100)
}

/// Raises the upper rate limit for the seller rate the user accepted for this swap (the asked
/// rate, plus the margin). Never lowers a limit, never touches the lower limit or the path budget.
pub(crate) fn with_accepted_seller_rate(limits: SwapFeeLimits, accepted: Option<u64>) -> SwapFeeLimits {
    match accepted.map(accepted_limit) {
        Some(rate) if rate > limits.max_rate.0 => SwapFeeLimits {
            max_rate: SwapFeeRate(rate),
            ..limits
        },
        _ => limits,
    }
}

/// Tells the GUI a seller was refused for its fee rate, with what it asked, our limit and our
/// own estimate (sat/kvB), so it can offer "accept and retry".
pub(crate) fn on_seller_fee_rate_too_high(env: &JNIEnv<'_>, swap_id: &str, refusal: RateAboveLimit, our_estimate: u64) {
    let listener = swap::util::get_swap_listener(env);
    let Ok(swap_id_bytes) = env.byte_array_from_slice(swap_id.as_bytes()) else {
        return;
    };
    let (Ok(seller), Ok(max), Ok(estimate)) = (
        i64::try_from(refusal.seller_rate),
        i64::try_from(refusal.our_max),
        i64::try_from(our_estimate),
    ) else {
        return;
    };
    if let JValue::Object(listener) = listener {
        let _ = env.call_method(
            listener,
            ON_SELLER_FEE_RATE_TOO_HIGH_METHOD,
            ON_SELLER_FEE_RATE_TOO_HIGH_SIGNATURE,
            &[JValue::from(swap_id_bytes), JValue::Long(seller), JValue::Long(max), JValue::Long(estimate)],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swap::protocol_v5::core::bitcoin::SwapFeeError;

    /// The parser reads the engine's own message, built here by the engine's own Display, so a
    /// change of wording fails this test instead of silently hiding the dialog.
    fn refusal(rate: u64, min: u64, max: u64) -> String {
        format!(
            "Protocol-5 setup failed: Swap rejected: Invalid swap fee proposal: {}",
            SwapFeeError::RateOutsideLimits {
                rate: SwapFeeRate(rate),
                min: SwapFeeRate(min),
                max: SwapFeeRate(max),
            }
        )
    }

    #[test]
    fn the_live_hermes_refusal_is_read() {
        assert_eq!(
            rate_above_limit(&refusal(6132, 1000, 3000)),
            Some(RateAboveLimit { seller_rate: 6132, our_max: 3000 })
        );
    }

    #[test]
    fn a_rate_below_our_minimum_is_never_offered() {
        assert_eq!(rate_above_limit(&refusal(400, 1000, 3000)), None);
    }

    #[test]
    fn other_failures_are_not_mistaken_for_it() {
        assert_eq!(rate_above_limit("Protocol-5 setup failed: Input verification was inconclusive."), None);
    }

    fn limits() -> SwapFeeLimits {
        SwapFeeLimits {
            min_rate: SwapFeeRate(1000),
            max_rate: SwapFeeRate(3000),
            max_path_fee: swap::bitcoin::Amount::from_sat(3_000),
        }
    }

    #[test]
    fn an_accepted_rate_raises_only_the_upper_limit() {
        // asked 6,132 sat/kvB, plus 10%
        let raised = with_accepted_seller_rate(limits(), Some(6132));
        assert_eq!(raised.max_rate, SwapFeeRate(6746));
        assert_eq!(raised.min_rate, SwapFeeRate(1000));
        assert_eq!(raised.max_path_fee, limits().max_path_fee);
    }

    #[test]
    fn without_acceptance_or_below_the_limit_nothing_changes() {
        assert_eq!(with_accepted_seller_rate(limits(), None), limits());
        assert_eq!(with_accepted_seller_rate(limits(), Some(2000)), limits());
        assert_eq!(accepted_limit(6132), 6746);
    }

    /// A seller asking more than what was accepted is still refused by the engine's own check.
    #[test]
    fn a_seller_above_the_accepted_rate_is_still_refused() {
        let raised = with_accepted_seller_rate(limits(), Some(6132));
        assert!(raised.validate_rate(SwapFeeRate(6746)).is_ok());
        assert!(raised.validate_rate(SwapFeeRate(6747)).is_err());
    }

    /// Source check (the buy path needs Tor): an accepted retry reserves the lock fee the seller's
    /// rate forces, or a whole-balance swap comes up short ("Insufficient funds", 2026-11-11).
    #[test]
    fn the_buy_path_reserves_the_seller_lock_fee_on_an_accepted_retry() {
        let buy = include_str!("buy_xmr_v5.rs");
        assert!(buy.contains("v5_lock_fee_for_seller_rate(rate)"));
        assert!(buy.contains("let max_givable = || bitcoin_wallet.max_giveable_v5_with_lock_fee_at_least(min_lock_fee);"));
        assert!(buy.contains("with_accepted_seller_rate(value, accepted_seller_fee_rate)"));
    }
}
