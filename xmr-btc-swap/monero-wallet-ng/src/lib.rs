pub mod confirmations;
pub mod double_spend;
pub mod empty;
pub mod hermes;
pub mod retry;
pub mod rpc;
pub mod scanner;
pub mod sweep;
pub mod util;
pub mod verify;

pub const HARDFORK_VERSION: u8 = 16;

/// Ring size for CLSAG Bulletproofs+ transactions.
///
/// Matches the value hardcoded inside `monero-oxide` for
/// `RctType::ClsagBulletproofPlus` (see `send/mod.rs` and `send/tx.rs`), which
/// is the protocol-required ring length for the current hard fork. There is
/// no exported constant upstream, so we mirror the magic number here.
pub const RING_LEN: u8 = 16;

/// Upper bound (in piconero per transaction weight unit) we accept from the
/// daemon's dynamic fee estimate.
///
/// Mainnet's fee estimate sits well under 100k pico/weight; regtest has been
/// observed returning ~1.2M pico/weight. This cap is loose enough to
/// accommodate regtest while still catching a daemon that reports an
/// obviously-wrong estimate. `wallet2` has no equivalent constant — it trusts
/// whatever the daemon returns.
///
/// The daemon is not trusted, and this cap is the only bound on the fee a
/// hostile daemon can make a swap sweep pay (protocol review S-4): it was 10M,
/// i.e. about 0.02 XMR on a typical sweep; at 2M it is about 0.004 XMR, with 20x
/// headroom over the mainnet figure above.
pub const MAX_FEE_PER_WEIGHT: u64 = 2_000_000;

#[cfg(test)]
mod fee_cap_tests {
    use super::*;

    /// A sweep is one input and two outputs, about 2,000 weight units; 4,000 is generous.
    const GENEROUS_SWEEP_WEIGHT: u64 = 4_000;
    const ONE_HUNDREDTH_XMR: u64 = 10_000_000_000;

    #[test]
    fn a_hostile_daemon_cannot_make_a_sweep_pay_a_hundredth_of_an_xmr() {
        assert!(MAX_FEE_PER_WEIGHT * GENEROUS_SWEEP_WEIGHT < ONE_HUNDREDTH_XMR);
    }

    #[test]
    fn the_cap_still_covers_the_regtest_estimate() {
        // regtest was observed at ~1.2M pico/weight (see the doc on the constant)
        assert!(MAX_FEE_PER_WEIGHT >= 1_200_000);
    }
}
