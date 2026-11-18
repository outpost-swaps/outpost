use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bitcoin::Amount;
use std::sync::Mutex;

use crate::asb::event_loop::LatestRate;
use crate::asb::liquidity::Reservations;
use crate::asb::RateSource;
use crate::protocol_v5::p2p::protocols::quote::{BidQuote, RefundPolicyWire};

const WALLET_RPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const RATE_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const RATE_WAIT_INTERVAL: Duration = Duration::from_millis(250);
/// How long a previously read unlocked balance may stand in for a live read
/// when quoting. The wallet RPC blocks every call while it refreshes over Tor
/// (about a minute in practice), and a failed quote makes the taker see an
/// empty reply. The quote is advisory: swap setup re-checks the live balance.
const QUOTE_BALANCE_MAX_AGE: Duration = Duration::from_secs(600);

/// Picks the unlocked balance to quote from: the live reading when there is
/// one, otherwise the last good reading if it is recent enough.
/// Returns `(unlocked_piconero, from_cache)`.
fn balance_for_quote(
    live: Result<u64>,
    last_good: Option<(Instant, u64)>,
    now: Instant,
    max_age: Duration,
) -> Result<(u64, bool)> {
    match live {
        Ok(balance) => Ok((balance, false)),
        Err(error) => match last_good {
            Some((read_at, balance)) if now.saturating_duration_since(read_at) <= max_age => {
                Ok((balance, true))
            }
            _ => Err(error),
        },
    }
}

pub struct V5QuoteProvider {
    rate: Arc<Mutex<RateSource>>,
    monero_wallet: Arc<crate::monero::Wallet>,
    liquidity: Arc<Reservations>,
    min_buy: Amount,
    max_buy: Amount,
    low_balance_warned: std::sync::atomic::AtomicBool,
    last_good_balance: Mutex<Option<(Instant, u64)>>,
}

impl V5QuoteProvider {
    pub fn new(
        rate: Arc<Mutex<RateSource>>,
        monero_wallet: Arc<crate::monero::Wallet>,
        liquidity: Arc<Reservations>,
        min_buy: Amount,
        max_buy: Amount,
    ) -> Self {
        Self {
            rate,
            monero_wallet,
            liquidity,
            min_buy,
            max_buy,
            low_balance_warned: std::sync::atomic::AtomicBool::new(false),
            last_good_balance: Mutex::new(None),
        }
    }

    fn zero_quote(price: crate::protocol_v5::core::bitcoin::Amount) -> BidQuote {
        BidQuote {
            price,
            min_quantity: crate::protocol_v5::core::bitcoin::Amount::from_sat(0),
            max_quantity: crate::protocol_v5::core::bitcoin::Amount::from_sat(0),
            refund_policy: RefundPolicyWire::FullRefund,
            reserve_proof: None,
        }
    }
}

#[async_trait::async_trait]
impl crate::protocol_v5::event_loop::MakerQuoteProvider for V5QuoteProvider {
    async fn quote(&self) -> Result<BidQuote> {
        // The first seller-list quote can arrive before the market feed's
        // first tick. Wait a bounded amount for a fresh rate instead of
        // publishing a failed quote that removes the ASB from the list.
        let deadline = Instant::now() + RATE_WAIT_TIMEOUT;
        let ask = loop {
            let rate = self.rate.lock().expect("rate mutex poisoned").latest_rate();
            match rate {
                Ok(rate) => break rate.ask().context("Failed to compute asking price")?,
                Err(error)
                    if error.to_string().contains("Rate is not yet available")
                        && Instant::now() < deadline =>
                {
                    tokio::time::sleep(RATE_WAIT_INTERVAL).await;
                }
                Err(error) => return Err(error).context("Failed to get latest rate"),
            }
        };
        let price = crate::protocol_v5::core::bitcoin::Amount::from_sat(ask.to_sat());

        let live = match tokio::time::timeout(WALLET_RPC_TIMEOUT, self.monero_wallet.get_balance())
            .await
        {
            Ok(Ok(balance)) => Ok(balance.unlocked_balance),
            Ok(Err(error)) => Err(error.context("Failed to fetch the Monero wallet balance")),
            Err(_) => Err(anyhow::anyhow!("Timed out waiting for the Monero wallet balance")),
        };
        let last_good = *self.last_good_balance.lock().expect("balance mutex poisoned");
        let (unlocked_balance, from_cache) =
            balance_for_quote(live, last_good, Instant::now(), QUOTE_BALANCE_MAX_AGE)?;
        if from_cache {
            tracing::warn!(
                "ASB_QUOTE_BALANCE_STALE: the Monero wallet did not answer; quoting from the last known balance"
            );
        } else {
            *self.last_good_balance.lock().expect("balance mutex poisoned") =
                Some((Instant::now(), unlocked_balance));
        }
        let xmr_balance = crate::monero::Amount::from_piconero(
            unlocked_balance
                .saturating_sub(self.liquidity.total().as_piconero())
                .saturating_sub(crate::monero::MONERO_FEE.as_piconero()),
        );
        let max_bitcoin_for_monero = xmr_balance
            .max_bitcoin_for_price(ask)
            .context("Bitcoin price times Monero balance overflowed")?;

        if self.min_buy > max_bitcoin_for_monero {
            if !self
                .low_balance_warned
                .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                tracing::warn!(
                    minimum = %self.min_buy,
                    maximum = %max_bitcoin_for_monero,
                    "ASB_XMR_BALANCE_LOW: not enough unlocked XMR for the configured minimum swap"
                );
            }
            return Ok(Self::zero_quote(price));
        }

        self.low_balance_warned
            .store(false, std::sync::atomic::Ordering::Relaxed);

        let max_quantity = if self.max_buy > max_bitcoin_for_monero {
            crate::protocol_v5::core::bitcoin::Amount::from_sat(max_bitcoin_for_monero.to_sat())
        } else {
            crate::protocol_v5::core::bitcoin::Amount::from_sat(self.max_buy.to_sat())
        };

        Ok(BidQuote {
            price,
            min_quantity: crate::protocol_v5::core::bitcoin::Amount::from_sat(
                self.min_buy.to_sat(),
            ),
            max_quantity,
            refund_policy: RefundPolicyWire::FullRefund,
            reserve_proof: None,
        })
    }
}

pub struct V5SnapshotProvider {
    bitcoin_wallet: Arc<crate::bitcoin::Wallet>,
    monero_wallet: Arc<crate::monero::Wallet>,
    external_redeem_address: Option<crate::bitcoin::Address>,
    liquidity: Arc<Reservations>,
}

impl V5SnapshotProvider {
    pub fn new(
        bitcoin_wallet: Arc<crate::bitcoin::Wallet>,
        monero_wallet: Arc<crate::monero::Wallet>,
        external_redeem_address: Option<crate::bitcoin::Address>,
        liquidity: Arc<Reservations>,
    ) -> Self {
        Self {
            bitcoin_wallet,
            monero_wallet,
            external_redeem_address,
            liquidity,
        }
    }
}

#[async_trait::async_trait]
impl crate::protocol_v5::event_loop::MakerWalletSnapshotProvider for V5SnapshotProvider {
    async fn snapshot(
        &self,
        btc_amount: crate::protocol_v5::core::bitcoin::Amount,
    ) -> Result<crate::protocol_v5::event_loop::MakerSnapshot> {
        let balance = tokio::time::timeout(WALLET_RPC_TIMEOUT, self.monero_wallet.get_balance())
            .await
            .context("Timed out waiting for the Monero wallet balance")?
            .context("Failed to fetch the Monero wallet balance")?;

        let redeem_address = match &self.external_redeem_address {
            Some(address) => address.clone(),
            None => self.bitcoin_wallet.new_address().await?,
        };
        let punish_address = match &self.external_redeem_address {
            Some(address) => address.clone(),
            None => self.bitcoin_wallet.new_address().await?,
        };
        let redeem_address = crate::protocol_v5::core::bitcoin::Address::from_str(
            &redeem_address.to_string(),
        )
        .context("Failed to convert the Bitcoin redeem address")?
        .assume_checked();
        let punish_address = crate::protocol_v5::core::bitcoin::Address::from_str(
            &punish_address.to_string(),
        )
        .context("Failed to convert the Bitcoin punish address")?
        .assume_checked();

        let fee_rate = self.bitcoin_wallet.swap_fee_rate().await?;
        let sat_per_kwu = fee_rate.to_sat_per_kwu();
        let estimated_fee_rate = ::bitcoin::FeeRate::from_sat_per_kwu(sat_per_kwu);
        let swap_fee_rate =
            crate::protocol_v5::core::bitcoin::SwapFeeRate::from_estimate(fee_rate)?;
        tracing::info!(
            estimated_sat_per_kwu = sat_per_kwu,
            advertised_swap_fee_rate_sat_per_kvb = swap_fee_rate.0,
            "V5_MAKER_FEE_SNAPSHOT"
        );
        let fee_limits = crate::protocol_v5::machine::common::swap_fee_limits(
            swap_fee_rate,
            btc_amount,
        )?;

        let snapshot =
            crate::protocol_v5::p2p::protocols::swap_setup::alice::WalletSnapshot::new(
                crate::protocol_v5::core::monero::Amount::from_pico(
                    balance.unlocked_balance,
                ),
                redeem_address,
                punish_address,
                estimated_fee_rate,
                swap_fee_rate,
                // 2.0x is the protocol minimum and still lets redeem outbid
                // the 1x cancel via RBF. Upstream defaults to 2.5x; we
                // advertise the floor to avoid overpaying the maker fee.
                crate::protocol_v5::core::bitcoin::RedeemFeeMultiplier::try_from(200)?,
                fee_limits,
            );

        Ok(crate::protocol_v5::event_loop::MakerSnapshot {
            snapshot,
            anti_spam_deposit: crate::protocol_v5::core::bitcoin::Amount::from_sat(0),
            hermes_enabled: false,
        })
    }

    async fn reserve_for_swap(
        &self,
        swap_id: uuid::Uuid,
        btc_amount: crate::protocol_v5::core::bitcoin::Amount,
        xmr_amount: crate::protocol_v5::core::monero::Amount,
    ) -> Result<bool> {
        let balance = match tokio::time::timeout(
            WALLET_RPC_TIMEOUT,
            self.monero_wallet.get_balance(),
        )
        .await
        {
            Ok(Ok(balance)) => balance,
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => anyhow::bail!("Timed out waiting for the Monero wallet balance"),
        };
        let unlocked = crate::monero::Amount::from_piconero(balance.unlocked_balance);
        let xmr = crate::monero::Amount::from_piconero(xmr_amount.as_pico());
        if let Err(error) =
            self.liquidity
                .reserve(swap_id, xmr, unlocked, crate::monero::MONERO_FEE)
        {
            tracing::warn!(%swap_id, %error, "ASB_LIQUIDITY_RESERVE_FAILED");
            return Ok(false);
        }
        let remaining = self
            .liquidity
            .available(unlocked, crate::monero::MONERO_FEE);
        tracing::info!(
            %swap_id,
            xmr = %xmr,
            buy = %bitcoin::Amount::from_sat(btc_amount.to_sat()),
            remaining = %remaining,
            "ASB_LIQUIDITY_RESERVED"
        );
        Ok(true)
    }

    async fn release_liquidity(&self, swap_id: uuid::Uuid) {
        if let Some(amount) = self.liquidity.release(swap_id) {
            tracing::info!(%swap_id, amount = %amount, "ASB_LIQUIDITY_RELEASED");
        }
    }
}

impl crate::protocol_v5::alice::LiquidityRelease for V5SnapshotProvider {
    fn release(&self, swap_id: uuid::Uuid) {
        if let Some(amount) = self.liquidity.release(swap_id) {
            tracing::info!(%swap_id, amount = %amount, "ASB_LIQUIDITY_RELEASED");
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("failed to read the market rate: {0}")]
pub struct V5RateError(String);

#[derive(Clone)]
pub struct V5RateSource {
    rate: Arc<Mutex<RateSource>>,
}

impl std::fmt::Debug for V5RateSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("V5RateSource").finish_non_exhaustive()
    }
}

impl V5RateSource {
    pub fn new(rate: Arc<Mutex<RateSource>>) -> Self {
        Self { rate }
    }
}

impl crate::protocol_v5::feed::LatestRate for V5RateSource {
    type Error = V5RateError;

    fn latest_rate(&mut self) -> std::result::Result<crate::protocol_v5::feed::Rate, Self::Error> {
        let rate = self
            .rate
            .lock()
            .map_err(|_| V5RateError("rate mutex poisoned".to_string()))?
            .latest_rate()
            .map_err(|error| V5RateError(error.to_string()))?
            .ask()
            .map_err(|error| V5RateError(error.to_string()))?;
        Ok(crate::protocol_v5::feed::Rate::new(
            crate::protocol_v5::core::bitcoin::Amount::from_sat(rate.to_sat()),
            rust_decimal::Decimal::ZERO,
        ))
    }
}

#[cfg(test)]
mod quote_balance_tests {
    use super::*;

    fn failing() -> Result<u64> {
        Err(anyhow::anyhow!("Timed out waiting for the Monero wallet balance"))
    }

    #[test]
    fn a_live_balance_is_used_when_available() {
        let now = Instant::now();
        let got = balance_for_quote(Ok(5), Some((now, 9)), now, Duration::from_secs(600)).unwrap();
        assert_eq!(got, (5, false));
    }

    #[test]
    fn a_recent_balance_stands_in_when_the_wallet_does_not_answer() {
        let read_at = Instant::now();
        let now = read_at + Duration::from_secs(120);
        let got = balance_for_quote(failing(), Some((read_at, 9)), now, Duration::from_secs(600))
            .unwrap();
        assert_eq!(got, (9, true));
    }

    #[test]
    fn an_old_balance_is_not_trusted() {
        let read_at = Instant::now();
        let now = read_at + Duration::from_secs(601);
        assert!(balance_for_quote(failing(), Some((read_at, 9)), now, Duration::from_secs(600)).is_err());
    }

    #[test]
    fn with_no_prior_balance_the_failure_is_reported() {
        assert!(balance_for_quote(failing(), None, Instant::now(), Duration::from_secs(600)).is_err());
    }
}
