//! Liquidity reservations for the ASB's unlocked Monero.
//!
//! The setup acceptance check observes the wallet's unlocked balance, but two
//! concurrently accepted swaps could both plan to spend it (the actual XMR
//! transfer only happens after the buyer's BTC lock confirms). Reservations
//! bridge that gap: a swap reserves its xmr amount + fee reserve at setup
//! acceptance and the reservation is released when the swap task exits
//! (success or failure). Available liquidity = unlocked - fee reserve - total
//! reserved.

use crate::monero::Amount;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Debug, Default)]
pub struct Reservations {
    inner: Mutex<HashMap<Uuid, Amount>>,
}

const MAX_RESERVATIONS: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum ReservationError {
    #[error("Insufficient unlocked liquidity: {unlocked} unlocked, {reserved} already reserved, {requested} requested, plus a Monero fee reserve for the new swap")]
    Insufficient {
        unlocked: Amount,
        reserved: Amount,
        requested: Amount,
    },
    #[error("Too many active or pending swap reservations (maximum {max}); refusing new setup")]
    TooMany { max: usize },
}

impl Reservations {
    pub fn new() -> Self {
        Self::default()
    }

    fn sum_of(&self, guard: &HashMap<Uuid, Amount>) -> Amount {
        guard.values().copied().fold(Amount::ZERO, |a, v| a + v)
    }

    /// Liquidity available for a new reservation given the wallet's unlocked
    /// balance and the per-swap fee reserve (MONERO_FEE).
    pub fn available(&self, unlocked: Amount, fee_reserve: Amount) -> Amount {
        let guard = self.inner.lock().unwrap();
        let total = self.sum_of(&guard);
        let net = unlocked
            .as_piconero()
            .checked_sub(fee_reserve.as_piconero())
            .and_then(|rest| rest.checked_sub(total.as_piconero()));
        Amount::from_piconero(net.unwrap_or(0))
    }

    /// Reserve liquidity for a swap; fails when the unlocked balance (minus
    /// the fee reserve and existing reservations) cannot cover it. The stored
    /// reservation includes this swap's fee reserve so concurrent swaps each
    /// pay for their own Monero transaction.
    pub fn reserve(
        &self,
        swap_id: Uuid,
        xmr_amount: Amount,
        unlocked: Amount,
        fee_reserve: Amount,
    ) -> Result<(), ReservationError> {
        let mut guard = self.inner.lock().unwrap();
        if guard.len() >= MAX_RESERVATIONS {
            return Err(ReservationError::TooMany {
                max: MAX_RESERVATIONS,
            });
        }
        let reserved = self.sum_of(&guard);
        let available = unlocked
            .as_piconero()
            .checked_sub(fee_reserve.as_piconero())
            .and_then(|rest| rest.checked_sub(reserved.as_piconero()));

        match available {
            Some(available) if available >= xmr_amount.as_piconero() => {
                let amount_with_fee = Amount::from_piconero(
                    xmr_amount
                        .as_piconero()
                        .saturating_add(fee_reserve.as_piconero()),
                );
                guard.insert(swap_id, amount_with_fee);
                Ok(())
            }
            _ => Err(ReservationError::Insufficient {
                unlocked,
                reserved,
                requested: xmr_amount,
            }),
        }
    }

    /// Reserve liquidity under a freshly minted key; used at setup
    /// acceptance where the buyer's swap id is not yet known. Returns an
    /// armed [`ReservationGuard`]: dropping it releases the reservation
    /// (covering aborted handshakes and negotiation timeouts), and on
    /// success [`ReservationGuard::disarm`] hands back the key for re-keying
    /// to the swap id via [`Reservations::transfer`].
    pub fn reserve_new(
        self: &Arc<Self>,
        xmr_amount: Amount,
        unlocked: Amount,
        fee_reserve: Amount,
    ) -> Result<ReservationGuard, ReservationError> {
        let key = Uuid::new_v4();
        self.reserve(key, xmr_amount, unlocked, fee_reserve)?;
        Ok(ReservationGuard {
            key,
            liquidity: self.clone(),
            armed: true,
        })
    }

    /// Reserve without a balance check (accounting only) - for swaps that
    /// are already committed (e.g. resurrected with a funded BTC lock).
    pub fn force_reserve(&self, swap_id: Uuid, xmr_amount: Amount, fee_reserve: Amount) {
        let amount_with_fee = Amount::from_piconero(
            xmr_amount
                .as_piconero()
                .saturating_add(fee_reserve.as_piconero()),
        );
        self.inner.lock().unwrap().insert(swap_id, amount_with_fee);
    }

    /// Re-key a reservation (e.g. from the setup-acceptance key to the
    /// buyer's swap id) without releasing the liquidity in between.
    pub fn transfer(&self, old_key: Uuid, new_key: Uuid) -> Option<Amount> {
        let mut guard = self.inner.lock().unwrap();
        guard.remove(&old_key).map(|amount| {
            guard.insert(new_key, amount);
            amount
        })
    }

    /// Total currently reserved amount.
    pub fn total(&self) -> Amount {
        let guard = self.inner.lock().unwrap();
        self.sum_of(&guard)
    }

    pub fn release(&self, swap_id: Uuid) -> Option<Amount> {
        self.inner.lock().unwrap().remove(&swap_id)
    }
}

/// Guard that releases its reservation on drop; see
/// [`Reservations::reserve_new`].
pub struct ReservationGuard {
    key: Uuid,
    liquidity: Arc<Reservations>,
    armed: bool,
}

impl ReservationGuard {
    /// Disarm the guard (the reservation stays in place) and return the key.
    pub fn disarm(mut self) -> Uuid {
        self.armed = false;
        self.key
    }

    pub fn key(&self) -> Uuid {
        self.key
    }
}

impl Drop for ReservationGuard {
    fn drop(&mut self) {
        if self.armed {
            if let Some(amount) = self.liquidity.release(self.key) {
                tracing::warn!(%amount, "Released liquidity reservation of aborted execution setup");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn xmr(piconero: u64) -> Amount {
        Amount::from_piconero(piconero)
    }

    #[test]
    fn reserves_within_available_balance() {
        let reservations = Reservations::new();
        let swap_id = Uuid::new_v4();

        reservations
            .reserve(swap_id, xmr(1_000_000_000_000), xmr(1_003_000_000_000), xmr(3_000_000_000))
            .expect("1 XMR must fit under 1.003 XMR minus the 0.003 XMR fee reserve");

        assert_eq!(reservations.release(swap_id), Some(xmr(1_003_000_000_000)));
        assert_eq!(reservations.release(swap_id), None, "release is one-shot");
    }

    #[test]
    fn second_swap_cannot_double_promise_the_same_liquidity() {
        let reservations = Reservations::new();
        let unlocked = xmr(1_003_000_000_000); // 1 XMR + 0.003 XMR fee reserve // exactly 1 XMR spendable + 0.003 XMR fee reserve

        reservations
            .reserve(Uuid::new_v4(), xmr(1_000_000_000_000), unlocked, xmr(3_000_000_000))
            .expect("first swap takes the full spendable XMR");

        let second = reservations.reserve(
            Uuid::new_v4(),
            xmr(1_000_000_000),
            unlocked,
            xmr(3_000_000_000),
        );
        assert!(
            second.is_err(),
            "the second swap must be refused: the XMR is promised"
        );
    }

    #[test]
    fn each_concurrent_swap_reserves_its_own_monero_fee() {
        let reservations = Arc::new(Reservations::new());
        let fee = xmr(3_000_000_000);
        let amount = xmr(500_000_000_000);

        // 1.005 XMR is only enough for one 0.5 XMR swap + two 0.003 fees.
        let first = reservations
            .reserve_new(amount, xmr(1_005_000_000_000), fee)
            .unwrap();
        assert!(
            reservations
                .reserve_new(amount, xmr(1_005_000_000_000), fee)
                .is_err(),
            "the second swap must reserve its own fee and be rejected"
        );
        assert_eq!(reservations.total(), xmr(503_000_000_000));
        drop(first);

        // 1.006 XMR covers both the two 0.5 XMR amounts and both fees.
        let first = reservations
            .reserve_new(amount, xmr(1_006_000_000_000), fee)
            .unwrap();
        let second = reservations
            .reserve_new(amount, xmr(1_006_000_000_000), fee)
            .unwrap();
        assert_eq!(reservations.total(), xmr(1_006_000_000_000));
        drop((first, second));
    }

    #[test]
    fn caps_pending_and_active_reservations() {
        let reservations = Arc::new(Reservations::new());
        let mut guards = Vec::new();
        for _ in 0..MAX_RESERVATIONS {
            guards.push(
                reservations
                    .reserve_new(xmr(0), xmr(0), xmr(0))
                    .expect("zero-value reservation fits"),
            );
        }

        let error = match reservations.reserve_new(xmr(0), xmr(0), xmr(0)) {
            Ok(_) => panic!("the reservation cap must reject another setup"),
            Err(error) => error,
        };
        assert!(matches!(error, ReservationError::TooMany { max: MAX_RESERVATIONS }));
        drop(guards);
    }

    #[test]
    fn released_liquidity_can_be_reserved_again() {
        let reservations = Reservations::new();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let unlocked = xmr(1_003_000_000_000); // 1 XMR + 0.003 XMR fee reserve

        reservations
            .reserve(first, xmr(1_000_000_000_000), unlocked, xmr(3_000_000_000))
            .unwrap();
        reservations.release(first);

        reservations
            .reserve(second, xmr(1_000_000_000_000), unlocked, xmr(3_000_000_000))
            .expect("released liquidity is reservable again");
    }

    #[test]
    fn available_never_counts_the_fee_reserve() {
        let reservations = Reservations::new();
        assert_eq!(
            reservations.available(xmr(3_000_000_000), xmr(3_000_000_000)),
            Amount::ZERO,
            "the fee reserve itself is not spendable liquidity"
        );
    }

    #[test]
    fn reserve_new_keys_transfer_to_the_swap_id() {
        let reservations = Arc::new(Reservations::new());
        let unlocked = xmr(1_003_000_000_000); // 1 XMR + 0.003 XMR fee reserve

        // setup acceptance: buyer's swap id not yet known
        let guard = reservations
            .reserve_new(xmr(1_000_000_000_000), unlocked, xmr(3_000_000_000))
            .expect("first swap takes the spendable XMR");
        assert!(
            reservations
                .reserve(Uuid::new_v4(), xmr(1), unlocked, xmr(3_000_000_000))
                .is_err(),
            "the liquidity is promised while the handshake runs"
        );

        // handshake completed: re-key to the swap id, liquidity stays reserved
        let swap_id = Uuid::new_v4();
        assert_eq!(
            reservations.transfer(guard.disarm(), swap_id),
            Some(xmr(1_003_000_000_000))
        );
        assert!(
            reservations
                .reserve(Uuid::new_v4(), xmr(1), unlocked, xmr(3_000_000_000))
                .is_err(),
            "still reserved under the swap id"
        );

        // swap task exited: release frees the liquidity
        assert_eq!(reservations.release(swap_id), Some(xmr(1_003_000_000_000)));
        reservations
            .reserve(Uuid::new_v4(), xmr(1_000_000_000_000), unlocked, xmr(3_000_000_000))
            .expect("released liquidity is reservable again");
    }

    #[test]
    fn disarmed_guard_does_not_release_on_drop() {
        let reservations = Arc::new(Reservations::new());
        let unlocked = xmr(1_003_000_000_000);

        // disarm consumes the guard without releasing; the key survives
        let key = {
            let guard = reservations
                .reserve_new(xmr(1_000_000_000_000), unlocked, xmr(3_000_000_000))
                .unwrap();
            guard.disarm()
        };

        // the reservation is still in place under `key`
        assert_eq!(reservations.transfer(key, Uuid::new_v4()).is_some(), true);
    }

    #[test]
    fn dropped_armed_guard_releases_the_reservation() {
        let reservations = Arc::new(Reservations::new());
        let unlocked = xmr(1_003_000_000_000);

        let guard = reservations
            .reserve_new(xmr(1_000_000_000_000), unlocked, xmr(3_000_000_000))
            .unwrap();
        drop(guard); // handshake aborted mid-negotiation

        reservations
            .reserve(Uuid::new_v4(), xmr(1_000_000_000_000), unlocked, xmr(3_000_000_000))
            .expect("an aborted handshake must not hold liquidity");
    }

    #[test]
    fn force_reserve_skips_the_balance_check() {
        let reservations = Reservations::new();
        // far over-committed, but the swap is already committed (resurrection)
        reservations.force_reserve(Uuid::new_v4(), xmr(1_000_000_000_000), xmr(3_000_000_000));
        assert_eq!(
            reservations.total(),
            xmr(1_003_000_000_000),
            "accounting-only reservation includes the per-swap fee reserve"
        );
    }

    #[test]
    fn two_concurrent_setups_cannot_both_reserve_the_same_liquidity() {
        use std::sync::Barrier;

        let reservations = Arc::new(Reservations::new());
        let unlocked = xmr(1_003_000_000_000); // 1 XMR + 0.003 XMR fee reserve
        let fee_reserve = xmr(3_000_000_000);
        let amount = xmr(1_000_000_000_000);
        let barrier = Arc::new(Barrier::new(2));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let reservations = reservations.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    // Both setup handlers observe the same unlocked balance,
                    // then race to reserve it - like two buyers connecting at
                    // the same time.
                    barrier.wait();
                    reservations
                        .reserve_new(amount, unlocked, fee_reserve)
                        .map(ReservationGuard::disarm)
                })
            })
            .collect();

        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();

        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            1,
            "exactly one concurrent setup may win the XMR"
        );
        assert_eq!(
            reservations.total(),
            xmr(1_003_000_000_000),
            "the winner's reservation includes its fee reserve and is the only one recorded"
        );
    }

    #[test]
    fn concurrent_setups_agree_on_the_number_of_swaps_that_fit() {
        use std::sync::Barrier;

        let reservations = Arc::new(Reservations::new());
        let unlocked = xmr(1_003_000_000_000); // 1 XMR + 0.003 XMR fee reserve
        let fee_reserve = xmr(3_000_000_000);
        let amount = xmr(400_000_000_000); // exactly two fit
        let contenders = 8;
        let barrier = Arc::new(Barrier::new(contenders));

        let handles: Vec<_> = (0..contenders)
            .map(|_| {
                let reservations = reservations.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    reservations
                        .reserve_new(amount, unlocked, fee_reserve)
                        .map(ReservationGuard::disarm)
                })
            })
            .collect();

        let successes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(Result::is_ok)
            .count();

        assert_eq!(successes, 2, "two swaps each reserving amount + fee fit");
        assert_eq!(reservations.total(), xmr(806_000_000_000));
        assert_eq!(
            reservations.available(unlocked, fee_reserve),
            xmr(194_000_000_000),
            "only the remainder after both swaps' fees is still available"
        );
        assert!(
            reservations
                .reserve_new(amount, unlocked, fee_reserve)
                .is_err(),
            "a ninth setup must be refused even though 0.2 XMR is free"
        );
    }

    #[test]
    fn concurrent_releases_restore_the_full_available_balance() {
        let reservations = Arc::new(Reservations::new());
        let unlocked = xmr(1_003_000_000_000);
        let fee_reserve = xmr(3_000_000_000);
        let amount = xmr(400_000_000_000);
        let barrier = Arc::new(std::sync::Barrier::new(2));

        let keys: Vec<Uuid> = (0..2)
            .map(|_| {
                reservations
                    .reserve_new(amount, unlocked, fee_reserve)
                    .unwrap()
                    .disarm()
            })
            .collect();
        assert_eq!(reservations.total(), xmr(806_000_000_000));

        let handles: Vec<_> = keys
            .into_iter()
            .map(|key| {
                let reservations = reservations.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    reservations.release(key)
                })
            })
            .collect();

        for handle in handles {
            assert_eq!(
                handle.join().unwrap(),
                Some(xmr(403_000_000_000)),
                "releasing returns the amount plus its fee reserve"
            );
        }

        assert_eq!(reservations.total(), Amount::ZERO);
        assert_eq!(
            reservations.available(unlocked, fee_reserve),
            xmr(1_000_000_000_000)
        );
        reservations
            .reserve_new(xmr(1_000_000_000_000), unlocked, fee_reserve)
            .expect("both released reservations must be spendable again");
    }

    proptest! {
        #[test]
        fn available_is_never_negative(
            unlocked in 0u64..20_000_000_000_000,
            fee_reserve in 0u64..10_000_000_000,
            reserved in 0u64..10_000_000_000,
        ) {
            let reservations = Reservations::new();
            if reserved > 0 {
                // seed a reservation larger than any realistic unlock to force
                // the underflow path
                let _ = reservations.reserve(
                    Uuid::new_v4(),
                    xmr(unlocked.saturating_add(fee_reserve).saturating_add(reserved)),
                    xmr(unlocked),
                    xmr(fee_reserve),
                );
            }
            let available = reservations.available(xmr(unlocked), xmr(fee_reserve));
            prop_assert!(available.as_piconero() <= unlocked.saturating_sub(fee_reserve));
        }
    }
}
