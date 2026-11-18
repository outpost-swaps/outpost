use crate::env;
use anyhow::{Context, Result, bail};
use bdk_wallet::rusqlite::Connection;
use bitcoin::bip32::Xpriv;
use bitcoin::{Address, Amount, FeeRate, Network, Psbt, Transaction, Txid, Weight};
use bitcoin_wallet::{BitcoinWallet, DojoFeeEstimator, StaticSeed, TestWalletBuilder, WalletAccount};
pub use bitcoin_wallet::StaticFeeRate;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use rust_decimal::{Decimal, prelude::{FromPrimitive, ToPrimitive}};
use rust_decimal_macros::dec;
use url::Url;

pub use bitcoin_wallet::primitives::EstimateFeeRate;
pub use bitcoin_wallet::{ScriptStatus, Subscription, Watchable};

const DOJO_CONFIRMATION_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Outpost's Bitcoin wallet. This is a direct wrapper around the shared
/// Bitcoin wallet implementation; it adds only app-specific constructors and
/// the fee/max-giveable surface used by the swap state machine.
pub struct Wallet<C = bitcoin_wallet::Client> {
    inner: bitcoin_wallet::Wallet<Connection, C>,
    network: Network,
    target_block: u32,
}

impl<C> Deref for Wallet<C> {
    type Target = bitcoin_wallet::Wallet<Connection, C>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<C> DerefMut for Wallet<C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<C: 'static> Wallet<C>
where
    bitcoin_wallet::Wallet<Connection, C>: BitcoinWallet + Clone,
{
    /// Clone the underlying wallet as the trait object used by the 5.0 engine.
    pub fn protocol_wallet(&self) -> Arc<dyn BitcoinWallet> {
        Arc::new(self.inner.clone())
    }
}

#[async_trait::async_trait]
impl<C> bitcoin_wallet::VerifyPsbtPrevouts for Wallet<C>
where
    C: Send + Sync,
    bitcoin_wallet::Wallet<Connection, C>: bitcoin_wallet::VerifyPsbtPrevouts + Send + Sync,
{
    async fn verify_psbt_prevouts(
        &self,
        psbt: &Psbt,
        cancel: &tokio::sync::watch::Receiver<bool>,
    ) -> std::result::Result<(), bitcoin_wallet::PsbtPrevoutVerificationError> {
        self.inner.verify_psbt_prevouts(psbt, cancel).await
    }
}

impl Wallet<bitcoin_wallet::Client> {
    pub async fn new(
        electrum_rpc_url: Url,
        electrum_socks5_proxy_string: &str,
        data_dir: impl AsRef<Path>,
        xprivkey: Xpriv,
        env_config: env::Config,
        target_block: usize,
    ) -> Result<Self> {
        Self::with_account(
            WalletAccount::Deposit,
            vec![electrum_rpc_url],
            electrum_socks5_proxy_string,
            data_dir,
            xprivkey,
            env_config,
            target_block,
        )
        .await
    }

    /// Deposit wallet over a pool of Electrum endpoints: the balancer fails over
    /// between them and replaces dead sessions, so one stalled onion server
    /// cannot stall a funded swap (swaps f7a162f7 and bf5f78fa, 2026-11-06/07).
    pub async fn new_with_urls(
        electrum_rpc_urls: Vec<Url>,
        electrum_socks5_proxy_string: &str,
        data_dir: impl AsRef<Path>,
        xprivkey: Xpriv,
        env_config: env::Config,
        target_block: usize,
    ) -> Result<Self> {
        Self::with_account(
            WalletAccount::Deposit,
            electrum_rpc_urls,
            electrum_socks5_proxy_string,
            data_dir,
            xprivkey,
            env_config,
            target_block,
        )
        .await
    }

    pub async fn new_refund(
        electrum_rpc_url: Url,
        electrum_socks5_proxy_string: &str,
        data_dir: impl AsRef<Path>,
        xprivkey: Xpriv,
        env_config: env::Config,
        target_block: usize,
    ) -> Result<Self> {
        Self::with_account(
            WalletAccount::Refund,
            vec![electrum_rpc_url],
            electrum_socks5_proxy_string,
            data_dir,
            xprivkey,
            env_config,
            target_block,
        )
        .await
    }

    pub async fn new_samourai_asb(
        electrum_rpc_url: Url,
        electrum_socks5_proxy_string: &str,
        data_dir: impl AsRef<Path>,
        xprivkey: Xpriv,
        env_config: env::Config,
        target_block: usize,
    ) -> Result<Self> {
        Self::new_samourai_asb_with_urls(
            vec![electrum_rpc_url],
            electrum_socks5_proxy_string,
            data_dir,
            xprivkey,
            env_config,
            target_block,
        )
        .await
    }

    pub async fn new_samourai_asb_with_urls(
        electrum_rpc_urls: Vec<Url>,
        electrum_socks5_proxy_string: &str,
        data_dir: impl AsRef<Path>,
        xprivkey: Xpriv,
        env_config: env::Config,
        target_block: usize,
    ) -> Result<Self> {
        Self::with_account(
            WalletAccount::Asb,
            electrum_rpc_urls,
            electrum_socks5_proxy_string,
            data_dir,
            xprivkey,
            env_config,
            target_block,
        )
        .await
    }

    async fn with_account(
        account: WalletAccount,
        electrum_rpc_urls: Vec<Url>,
        electrum_socks5_proxy_string: &str,
        data_dir: impl AsRef<Path>,
        xprivkey: Xpriv,
        env_config: env::Config,
        target_block: usize,
    ) -> Result<Self> {
        let data_dir = data_dir.as_ref();
        let network = wallet_network(env_config.bitcoin_network);
        let dojo = Self::dojo_fee_estimator(data_dir, electrum_socks5_proxy_string);
        let tor_proxy = electrum_tor_proxy(electrum_socks5_proxy_string, &electrum_rpc_urls)?;

        let inner = bitcoin_wallet::Wallet::with_sqlite(
            &StaticSeed::new(xprivkey),
            network,
            account,
            dojo,
            false,
            &electrum_rpc_urls
                .iter()
                .map(|url| url.to_string())
                .collect::<Vec<_>>(),
            tor_proxy,
            data_dir,
            env_config.bitcoin_finality_confirmations,
            target_block as u32,
            env_config.bitcoin_sync_interval(),
            None,
        )
        .await?;

        Ok(Self {
            inner,
            network,
            target_block: target_block as u32,
        })
    }

    fn dojo_fee_estimator(
        data_dir: &Path,
        electrum_socks5_proxy_string: &str,
    ) -> Option<Arc<DojoFeeEstimator>> {
        let tor_port = electrum_socks5_proxy_string
            .rsplit(':')
            .next()
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or(0);
        let estimator = DojoFeeEstimator::from_properties_near(data_dir, tor_port).map(Arc::new);
        if estimator.is_none() && tor_port == 0 {
            tracing::warn!(
                "No Tor SOCKS port is configured, so a remote Dojo fee estimator is not created; fee estimates will fail closed or use Electrum"
            );
        }
        estimator
    }

    pub async fn require_dojo_fee_estimator(
        &self,
        dojo_url: Url,
        api_key: String,
        tor_socks5_port: u16,
    ) -> Result<()> {
        let estimator = Arc::new(
            DojoFeeEstimator::new(dojo_url, api_key, tor_socks5_port)
                .context("Failed to configure the required Dojo fee estimator")?,
        );
        self.inner.set_dojo_fee_estimator(estimator).await;
        // Dojo is preferred, but a Dojo that is up with an unusable fee map
        // (e.g. testnet returning zero/no-confidence rates) must not block
        // swaps and refunds forever. Keep Electrum available as the fallback;
        // the wallet logs when it takes that path.
        Ok(())
    }

    pub fn require_dojo_fee_source(&self) {
        self.inner.require_dojo_fee_source();
    }
}

/// Outpost's testnet mode targets testnet4, but the protocol's environment
/// still uses the legacy `Testnet` name for wire compatibility. The wallet
/// layer needs the real chain so BDK anchors its checkpoints to the testnet4
/// genesis instead of testnet3.
fn wallet_network(network: Network) -> Network {
    match network {
        Network::Testnet => Network::Testnet4,
        other => other,
    }
}

/// The SOCKS proxy for the Electrum servers. Local servers (a test node or the
/// local pool forwarder, which dials Tor itself) need none. Any remote server
/// requires a configured proxy: an empty one is an error, never a direct
/// connection that would show the user's address to the server.
fn electrum_tor_proxy(proxy: &str, urls: &[Url]) -> Result<Option<String>> {
    if urls.iter().all(bitcoin_wallet::tor_policy::url_is_local) {
        return Ok(None);
    }
    if proxy.trim().is_empty() {
        bail!("refusing to sync with a remote Electrum server without Tor: no SOCKS proxy is configured");
    }
    Ok(Some(proxy.to_string()))
}

#[cfg(test)]
mod electrum_tor_proxy_tests {
    use super::*;

    fn urls(list: &[&str]) -> Vec<Url> {
        list.iter().map(|u| Url::parse(u).unwrap()).collect()
    }

    #[test]
    fn a_remote_server_without_a_proxy_is_refused() {
        assert!(electrum_tor_proxy("", &urls(&["tcp://abcdefghijklmnop.onion:50001"])).is_err());
        assert!(electrum_tor_proxy("  ", &urls(&["ssl://electrum.example:50002"])).is_err());
    }

    #[test]
    fn a_mixed_pool_with_one_remote_server_needs_the_proxy() {
        assert!(electrum_tor_proxy("", &urls(&["tcp://127.0.0.1:50001", "tcp://abcdefghijklmnop.onion:50001"])).is_err());
    }

    #[test]
    fn a_remote_server_uses_the_configured_proxy() {
        assert_eq!(
            electrum_tor_proxy("127.0.0.1:9050", &urls(&["tcp://abcdefghijklmnop.onion:50001"])).unwrap().as_deref(),
            Some("127.0.0.1:9050")
        );
    }

    #[test]
    fn local_servers_need_no_proxy() {
        assert_eq!(electrum_tor_proxy("", &urls(&["tcp://127.0.0.1:50001"])).unwrap(), None);
        assert_eq!(electrum_tor_proxy("127.0.0.1:9050", &urls(&["tcp://127.0.0.1:50001"])).unwrap(), None);
    }
}

impl Wallet<bitcoin_wallet::Client> {
    pub async fn broadcast(
        &self,
        transaction: Transaction,
        kind: &str,
    ) -> Result<(Txid, Subscription)> {
        self.inner.broadcast(transaction, kind).await
    }

    pub async fn broadcast_if_unseen(
        &self,
        transaction: Transaction,
        kind: &str,
    ) -> Result<(Txid, Subscription)> {
        self.inner.broadcast_if_unseen(transaction, kind).await
    }

    pub async fn get_raw_transaction(&self, txid: Txid) -> Result<Transaction> {
        self.inner
            .get_raw_transaction(txid)
            .await?
            .map(|tx| (*tx).clone())
            .with_context(|| format!("Could not get raw tx with id: {}", txid))
    }

    pub async fn status_of_script(&self, tx: &dyn Watchable) -> Result<ScriptStatus> {
        self.inner.status_of_script(tx).await
    }

    pub async fn subscribe_to(&self, tx: impl Watchable + Send + 'static) -> Subscription {
        self.inner.subscribe_to(Box::new(tx)).await
    }

    pub async fn wallet_export(
        &self,
        role: &str,
    ) -> Result<bdk_wallet::export::FullyNodedExport> {
        self.inner.wallet_export(role).await
    }

    pub async fn transaction_fee(&self, txid: Txid) -> Result<Amount> {
        self.inner.transaction_fee(txid).await
    }

    pub async fn sync(&self) -> Result<()> {
        self.inner.sync().await
    }

    pub async fn get_tx(&self, txid: Txid) -> Result<Option<Transaction>> {
        Ok(self.inner.get_tx(txid).await?.map(|tx| (*tx).clone()))
    }

    /// Seeds unconfirmed transactions from the paired Dojo so an incoming
    /// deposit is visible before the Electrum script-history sync completes.
    pub async fn seed_from_dojo(&self) -> Result<usize> {
        self.inner.seed_from_dojo().await
    }
}

impl<C> Wallet<C>
where
    C: EstimateFeeRate + Send + Sync + 'static,
{
    pub fn get_network(&self) -> Network {
        self.network
    }

    pub fn target_block(&self) -> u32 {
        self.target_block
    }

    pub async fn sign_and_finalize(&self, psbt: Psbt) -> Result<Transaction> {
        self.inner.sign_and_finalize(psbt).await
    }

    pub async fn balance(&self) -> Result<Amount> {
        self.inner.balance().await
    }

    pub async fn new_address(&self) -> Result<Address> {
        self.inner.new_address().await
    }

    pub async fn new_change_address(&self) -> Result<Address> {
        self.inner.new_change_address().await
    }

    pub async fn seed_address_cache(
        &self,
        receive_index: u32,
        change_index: u32,
    ) -> Result<()> {
        // Use the highest index this wallet has actually used. The revealed
        // index includes the previous call's gap, so using it made every run
        // reveal another 20 addresses per keychain, forever.
        let own_receive = self
            .inner
            .next_used_derivation_index(bdk_wallet::KeychainKind::External)
            .await;
        let own_change = self
            .inner
            .next_used_derivation_index(bdk_wallet::KeychainKind::Internal)
            .await;
        let Some(cache_size) =
            address_cache_size_with_own(receive_index, change_index, own_receive, own_change)
        else {
            return Ok(());
        };
        self.inner
            .seed_address_cache(receive_index, change_index, cache_size)
            .await
    }

    pub async fn send_to_address(
        &self,
        address: Address,
        amount: Amount,
        change_override: Option<Address>,
    ) -> Result<Psbt> {
        self.inner
            .send_to_address_dynamic_fee(address, amount, change_override, None)
            .await
    }

    pub async fn send_to_address_with_fee(
        &self,
        address: Address,
        amount: Amount,
        fee: Amount,
        change_override: Option<Address>,
    ) -> Result<Psbt> {
        self.inner
            .send_to_address(address, amount, fee, change_override, None)
            .await
    }

    pub async fn max_giveable(&self, locking_script_size: usize) -> Result<Amount> {
        let (wallet_max_giveable, reserved_fee) =
            self.max_giveable_with_fee(locking_script_size).await?;
        if wallet_max_giveable == Amount::ZERO {
            return Ok(Amount::ZERO);
        }

        // Match the pre-v5 behaviour: reserve the same policy fee the lock
        // transaction will use. The inner wallet's generic max-giveable has a
        // flat 1,000-sat minimum, which does not apply to swap locks.
        let sent = wallet_max_giveable + reserved_fee;
        let policy_fee = self.estimate_lock_policy_fee(wallet_max_giveable).await?;
        Ok(Amount::from_sat(
            sent.to_sat().saturating_sub(policy_fee.to_sat()),
        ))
    }

    /// The v5 buyer's spendable amount must reserve the v5 lock fee, not the
    /// legacy lock fee. The two lock transactions have different weights; using
    /// the legacy fee here under-reserves by the weight delta and makes
    /// `bob_make_psbt` fail input selection with an amount shortfall.
    #[cfg(feature = "protocol-v5")]
    pub async fn max_giveable_v5(&self) -> Result<Amount> {
        self.max_giveable_v5_with_lock_fee_at_least(Amount::ZERO).await
    }

    /// Like [`Self::max_giveable_v5`], but reserves at least `min_lock_fee` for the lock. Used
    /// when the seller's rate is already known (an accepted retry, matrix S-01): the buyer must
    /// then pay the lock fee that rate implies (`v5_lock_fee_for_seller_rate`), and reserving
    /// only our own fee left a whole-balance swap short ("Insufficient funds", 2026-11-11).
    #[cfg(feature = "protocol-v5")]
    pub async fn max_giveable_v5_with_lock_fee_at_least(&self, min_lock_fee: Amount) -> Result<Amount> {
        let locking_script_size =
            crate::protocol_v5::core::bitcoin::TxLock::script_size();
        let (wallet_max_giveable, reserved_fee) =
            self.max_giveable_with_fee(locking_script_size).await?;
        if wallet_max_giveable == Amount::ZERO {
            return Ok(Amount::ZERO);
        }

        let sent = wallet_max_giveable + reserved_fee;
        let policy_fee = self.estimate_v5_lock_policy_fee().await?.max(min_lock_fee);
        Ok(Amount::from_sat(
            sent.to_sat().saturating_sub(policy_fee.to_sat()),
        ))
    }

    /// Match the v5 maker's integer-sat/vB fee rounding.
    ///
    /// Alice derives her lock-floor rate as
    /// `ceil(ceil(advertised_sat_per_vb) * 1.25)`, then multiplies by the
    /// 485-WU v5 lock. Bob's legacy fractional-rate estimate at 1 sat/vB
    /// reserves 154 sats, while this formula reserves 243; the 89-sat gap
    /// makes `bob_make_psbt` fail input selection.
    #[cfg(feature = "protocol-v5")]
    async fn estimate_v5_lock_policy_fee(&self) -> Result<Amount> {
        let target_two = self
            .inner
            .fee_rate_at(self.target_block as u32)
            .await?;
        let target_one = self.inner.fee_rate_at(1).await?;
        let rate = if target_two > target_one {
            target_two
        } else {
            target_one
        };
        let sats_per_vb = rate.to_sat_per_vb_ceil();
        let buffered_sats_per_vb = (sats_per_vb as f64
            * f64::from(crate::bitcoin::LOCK_REDEEM_FEE_RATE_MULTIPLIER))
        .ceil() as u64;
        let buffered_rate = bitcoin::FeeRate::from_sat_per_vb(buffered_sats_per_vb)
            .context("Failed to compute v5 lock fee rate")?;
        let policy_fee = buffered_rate
            .checked_mul_by_weight(crate::protocol_v5::core::bitcoin::TxLock::weight())
            .context("Failed to compute v5 lock fee")?;
        Ok(v5_lock_fee_for_any_maker(policy_fee))
    }

    /// The single lock-fee policy shared by `max_giveable` and swap setup.
    ///
    /// `docs/FEES.md`: max(local target-2, local target-1) × 1.25. Computing
    /// this in more than one place is what produced the 153-vs-243 reserve
    /// regression: setup selected 243 but max_giveable reserved 153.
    pub async fn estimate_lock_policy_fee(&self, transfer_amount: Amount) -> Result<Amount> {
        self.estimate_lock_policy_fee_for_vsize(
            crate::bitcoin::TxLock::vsize(),
            transfer_amount,
        )
        .await
    }

    async fn estimate_lock_policy_fee_for_vsize(
        &self,
        vsize: usize,
        transfer_amount: Amount,
    ) -> Result<Amount> {
        let target_two = self
            .estimate_fee_vsize_at_with_rate_multiplier(
                vsize,
                transfer_amount,
                self.target_block as usize,
                crate::bitcoin::LOCK_REDEEM_FEE_RATE_MULTIPLIER,
            )
            .await?;
        let target_one = self
            .estimate_fee_vsize_at_with_rate_multiplier(
                vsize,
                transfer_amount,
                1,
                crate::bitcoin::LOCK_REDEEM_FEE_RATE_MULTIPLIER,
            )
            .await?;
        Ok(target_two.max(target_one))
    }

    /// Like [`Self::max_giveable`], but also returns the fee the wallet
    /// reserved. Callers that know the real transaction fee can add the
    /// difference back to the spendable amount instead of leaving change.
    pub async fn max_giveable_with_fee(
        &self,
        locking_script_size: usize,
    ) -> Result<(Amount, Amount)> {
        let (max_giveable, fee, _) = self
            .max_giveable_with_fee_and_weight(locking_script_size)
            .await?;
        Ok((max_giveable, fee))
    }

    /// Like [`Self::max_giveable`], but also returns the signed weight of the
    /// dummy drain transaction so callers can price the exact input set.
    pub async fn max_giveable_with_fee_and_weight(
        &self,
        locking_script_size: usize,
    ) -> Result<(Amount, Amount, Weight)> {
        self.inner
            .max_giveable_with_weight(locking_script_size, None)
            .await
    }

    pub async fn estimate_fee(
        &self,
        weight: usize,
        transfer_amount: Amount,
    ) -> Result<Amount> {
        let fee_rate = self.inner.fee_rate_at(self.target_block).await?;
        let min_relay_fee = self.inner.min_relay_fee_rate().await?;
        estimate_fee_weight(
            weight,
            transfer_amount,
            fee_rate_decimal(fee_rate),
            min_relay_fee,
        )
    }

    pub async fn estimate_fee_vsize(
        &self,
        vsize: usize,
        transfer_amount: Amount,
    ) -> Result<Amount> {
        let fee_rate = self.inner.fee_rate_at(self.target_block).await?;
        let min_relay_fee = self.inner.min_relay_fee_rate().await?;
        estimate_fee_vsize(
            vsize,
            transfer_amount,
            fee_rate_decimal(fee_rate),
            min_relay_fee,
        )
    }

    pub async fn estimate_fee_at(
        &self,
        weight: usize,
        transfer_amount: Amount,
        target_block: usize,
    ) -> Result<Amount> {
        let fee_rate = self.inner.fee_rate_at(target_block as u32).await?;
        let min_relay_fee = self.inner.min_relay_fee_rate().await?;
        estimate_fee_weight(
            weight,
            transfer_amount,
            fee_rate_decimal(fee_rate),
            min_relay_fee,
        )
    }

    pub async fn estimate_lock_fee(&self, transfer_amount: Amount) -> Result<Amount> {
        let fee_rate = self.inner.fee_rate_at(self.target_block).await?;
        let min_relay_fee = self.inner.min_relay_fee_rate().await?;
        if fee_rate.to_sat_per_kwu() == 0 && min_relay_fee.to_sat_per_kwu() == 0 {
            return Ok(Amount::ZERO);
        }
        let fee_rate = fee_rate_decimal(fee_rate)
            * Decimal::from_f32(crate::bitcoin::LOCK_REDEEM_FEE_RATE_MULTIPLIER)
                .expect("valid multiplier");
        estimate_fee_vsize(
            crate::bitcoin::TxLock::vsize(),
            transfer_amount,
            fee_rate,
            min_relay_fee,
        )
    }

    pub async fn estimate_fee_vsize_at(
        &self,
        vsize: usize,
        transfer_amount: Amount,
        target_block: usize,
    ) -> Result<Amount> {
        let fee_rate = self.inner.fee_rate_at(target_block as u32).await?;
        let min_relay_fee = self.inner.min_relay_fee_rate().await?;
        estimate_fee_vsize(
            vsize,
            transfer_amount,
            fee_rate_decimal(fee_rate),
            min_relay_fee,
        )
    }

    pub async fn estimate_fee_vsize_with_rate_multiplier(
        &self,
        vsize: usize,
        transfer_amount: Amount,
        rate_multiplier: f32,
    ) -> Result<Amount> {
        self.estimate_fee_vsize_at_with_rate_multiplier(
            vsize,
            transfer_amount,
            self.target_block as usize,
            rate_multiplier,
        )
        .await
    }

    pub async fn estimate_fee_vsize_at_with_rate_multiplier(
        &self,
        vsize: usize,
        transfer_amount: Amount,
        target_block: usize,
        rate_multiplier: f32,
    ) -> Result<Amount> {
        let source_fee_rate = self.inner.fee_rate_at(target_block as u32).await?;
        let min_relay_fee = self.inner.min_relay_fee_rate().await?;
        let source_fee_rate = fee_rate_decimal(source_fee_rate);
        let fee_rate = source_fee_rate
            * Decimal::from_f32(rate_multiplier).context("Failed to parse fee multiplier")?;
        let estimated_fee = estimate_fee_vsize(vsize, transfer_amount, fee_rate, min_relay_fee)?;
        tracing::info!(
            target_block,
            vsize,
            source_sat_per_vb = %source_fee_rate,
            multiplier = rate_multiplier as f64,
            buffered_sat_per_vb = %fee_rate,
            min_relay_sat_per_vb = %fee_rate_decimal(min_relay_fee),
            fee_sats = estimated_fee.to_sat(),
            "V5_BOB_FEE_ESTIMATE"
        );
        Ok(estimated_fee)
    }

}

fn fee_rate_decimal(fee_rate: FeeRate) -> Decimal {
    Decimal::from(fee_rate.to_sat_per_kwu()) / dec!(250)
}

fn estimate_fee_weight(
    weight: usize,
    transfer_amount: Amount,
    fee_rate: Decimal,
    min_relay_fee: FeeRate,
) -> Result<Amount> {
    estimate_fee(Decimal::from(weight), transfer_amount, fee_rate, min_relay_fee)
}

fn estimate_fee_vsize(
    vsize: usize,
    transfer_amount: Amount,
    fee_rate: Decimal,
    min_relay_fee: FeeRate,
) -> Result<Amount> {
    estimate_fee(
        Decimal::from(vsize) * dec!(4.0),
        transfer_amount,
        fee_rate,
        min_relay_fee,
    )
}

fn estimate_fee(
    weight: Decimal,
    transfer_amount: Amount,
    fee_rate: Decimal,
    min_relay_fee: FeeRate,
) -> Result<Amount> {
    if transfer_amount.to_sat() <= 546 {
        bail!("Amounts needs to be greater than Bitcoin dust amount.")
    }
    if fee_rate <= Decimal::ZERO {
        bail!("Fee rate needs to be > 0")
    }
    if fee_rate > Decimal::from(100_000_000)
        || min_relay_fee.to_sat_per_vb_ceil() > 100_000_000
    {
        bail!("A fee_rate or min_relay_fee of > 1BTC does not make sense")
    }

    let vsize = weight / dec!(4.0);
    let sats_per_vbyte = vsize * fee_rate;
    let transfer_amount = Decimal::from(transfer_amount.to_sat());
    let max_allowed_fee = (transfer_amount * dec!(0.20)).min(Decimal::from(100_000_u64));
    let min_relay_fee = if min_relay_fee.to_sat_per_kwu() == 0 {
        Decimal::ONE
    } else {
        Decimal::from(min_relay_fee.to_sat_per_kwu() * 4) * vsize / dec!(1000)
            * dec!(1.25)
    };

    let recommended_fee = if sats_per_vbyte < min_relay_fee {
        min_relay_fee
            .ceil()
            .to_u64()
            .context("Minimum relay fee overflow")?
    } else if sats_per_vbyte > max_allowed_fee {
        max_allowed_fee.to_u64().context("Maximum fee overflow")?
    } else {
        sats_per_vbyte.ceil().to_u64().context("Fee overflow")?
    };

    Ok(Amount::from_sat(recommended_fee))
}

fn address_cache_size_with_own(
    receive_index: u32,
    change_index: u32,
    own_receive: u32,
    own_change: u32,
) -> Option<u32> {
    address_cache_size(receive_index.max(own_receive), change_index.max(own_change))
}

fn address_cache_size(receive_index: u32, change_index: u32) -> Option<u32> {
    const CACHE_GAP: u32 = 20;

    let highest = receive_index.max(change_index);
    if highest == 0 {
        return None;
    }

    Some(highest.saturating_add(CACHE_GAP))
}

#[cfg(test)]
pub struct WalletBuilder {
    inner: TestWalletBuilder,
    fees: Option<(f32, u64)>,
}

#[cfg(test)]
impl WalletBuilder {
    pub fn new(utxo_amount: u64) -> Self {
        Self {
            inner: TestWalletBuilder::new(utxo_amount),
            fees: None,
        }
    }

    pub fn with_zero_fees(self) -> Self {
        Self {
            inner: self.inner.with_zero_fees(),
            fees: Some((0.0, 0)),
        }
    }

    pub fn with_fees(self, sats_per_vb: f32, min_relay_fee_sats: u64) -> Self {
        Self {
            inner: self.inner,
            fees: Some((sats_per_vb, min_relay_fee_sats)),
        }
    }

    pub fn with_num_utxos(self, number: u8) -> Self {
        Self {
            inner: self.inner.with_num_utxos(number),
            fees: self.fees,
        }
    }

    pub fn with_key(self, key: Xpriv) -> Self {
        Self {
            inner: self.inner.with_key(key),
            fees: self.fees,
        }
    }

    pub fn build(self) -> Wallet<StaticFeeRate> {
        let inner = match self.fees {
            Some((fee_rate, min_relay_fee_sats)) => {
                let fee_rate =
                    FeeRate::from_sat_per_kwu((fee_rate * 250.0).round() as u64);
                let min_relay = Amount::from_sat(
                    min_relay_fee_sats.div_ceil(1000).max(1),
                );
                futures::executor::block_on(
                    self.inner
                        .build_with_fee_strategy(StaticFeeRate::new(fee_rate, min_relay)),
                )
            }
            None => futures::executor::block_on(self.inner.build()),
        };
        Wallet {
            inner,
            network: Network::Regtest,
            target_block: 1,
        }
    }

    pub fn build_with_fee_strategy<C>(self, client: C) -> Wallet<C>
    where
        C: EstimateFeeRate + Send + Sync + Clone + 'static,
    {
        let inner = futures::executor::block_on(self.inner.build_with_fee_strategy(client));
        Wallet {
            inner,
            network: Network::Regtest,
            target_block: 1,
        }
    }

    pub fn target_aware_fee_rate() -> TargetAwareFeeRate {
        TargetAwareFeeRate {
            requested_targets: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

#[cfg(test)]
#[derive(Clone)]
pub struct TargetAwareFeeRate {
    pub requested_targets: Arc<std::sync::Mutex<Vec<u32>>>,
}

#[cfg(test)]
impl EstimateFeeRate for TargetAwareFeeRate {
    async fn estimate_feerate(&self, target_block: u32) -> Result<bitcoin::FeeRate> {
        self.requested_targets.lock().unwrap().push(target_block);
        let sats_per_vb = match target_block {
            1 => 20,
            2..=3 => 10,
            4..=8 => 3,
            _ => 1,
        };
        Ok(bitcoin::FeeRate::from_sat_per_vb(sats_per_vb).unwrap())
    }

    async fn min_relay_fee(&self) -> Result<bitcoin::FeeRate> {
        Ok(bitcoin::FeeRate::from_sat_per_vb(1).unwrap())
    }
}

/// The lowest lock fee an Eigenwallet 5.0.2 maker accepts (matrix S-01, WORKLOG §457).
///
/// Their maker requires at least half its own estimate for the lock, and its estimate comes from
/// `bitcoin_wallet::estimate_fee`, which never goes below the wallet's flat
/// `MIN_ABSOLUTE_TX_FEE` (1,000 sats). So on a quiet chain it rejects any lock below 500 sats
/// ("Transaction fee is too low compared to our estimate (364 sats vs 1000 sats)", live
/// testnet, 2026-11-11). Our own maker has no flat floor (§320); this only lifts what an
/// Outpost buyer pays, so it can buy from either.
pub const V5_LOCK_FEE_MIN_FOR_ANY_MAKER: Amount =
    Amount::from_sat(bitcoin_wallet::MIN_ABSOLUTE_TX_FEE_SATS / 2);

/// The lock fee a seller's advertised rate (Message1, sat/kvB) forces on the buyer. Mirrors the v5
/// Bob machine exactly (`protocol-v5/swap-machine/src/bob/mod.rs`, `alice_lock_fee`): the rate in
/// whole sat/vB rounded up, times 1.25 rounded up, times the v5 lock weight.
#[cfg(feature = "protocol-v5")]
pub fn v5_lock_fee_for_seller_rate(rate_sat_per_kvb: u64) -> Result<Amount> {
    let seller_rate = bitcoin::FeeRate::from_sat_per_kwu(rate_sat_per_kvb / 4);
    let buffered = bitcoin::FeeRate::from_sat_per_vb((seller_rate.to_sat_per_vb_ceil() as f64 * 1.25).ceil() as u64)
        .context("Failed to compute the seller-derived lock fee rate")?;
    buffered
        .checked_mul_by_weight(crate::protocol_v5::core::bitcoin::TxLock::weight())
        .context("Failed to compute the seller-derived lock fee")
}

/// Our Dojo-based lock fee, lifted to [`V5_LOCK_FEE_MIN_FOR_ANY_MAKER`] when below it.
pub fn v5_lock_fee_for_any_maker(policy_fee: Amount) -> Amount {
    policy_fee.max(V5_LOCK_FEE_MIN_FOR_ANY_MAKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn testnet_wallet_uses_testnet4_genesis() {
        assert_eq!(wallet_network(Network::Testnet), Network::Testnet4);
        assert_eq!(wallet_network(Network::Testnet4), Network::Testnet4);
        assert_eq!(wallet_network(Network::Bitcoin), Network::Bitcoin);
        assert_eq!(wallet_network(Network::Regtest), Network::Regtest);
    }

    #[test]
    fn swap_deposit_descriptor_targets_swaps_deposit_account() {
        let xprv = Xpriv::new_master(Network::Testnet, &[0u8; 64]).unwrap();
        let descriptor = bitcoin_wallet::SamouraiBip84(
            xprv,
            bdk_wallet::KeychainKind::External,
            WalletAccount::Deposit,
        );
        use bdk_wallet::template::DescriptorTemplate;
        let (descriptor, _, _) = descriptor.build(Network::Testnet).unwrap();
        let descriptor = descriptor.to_string();
        assert!(descriptor.contains("/2147483643'"));
    }

    #[test]
    fn high_swap_address_indices_extend_the_scanned_cache() {
        assert_eq!(address_cache_size(46, 6), Some(66));
        assert_eq!(address_cache_size(0, 0), None);
        assert_eq!(address_cache_size(1, 0), Some(21));
    }

    #[test]
    fn own_wallet_index_extends_the_scanned_cache() {
        assert_eq!(address_cache_size_with_own(0, 0, 46, 6), Some(66));
        assert_eq!(address_cache_size_with_own(46, 6, 0, 0), Some(66));
        assert_eq!(address_cache_size_with_own(0, 0, 0, 0), None);
        assert_eq!(address_cache_size_with_own(10, 2, 5, 4), Some(30));
    }

    // Every run seeded the address cache from the wallet's own next index plus a
    // 20-address gap. That index already contained the previous run's gap, so each
    // run revealed 20 more addresses on both keychains and never stopped: the
    // deposit wallet reached 676 addresses per keychain (1352 scripts) with real
    // indices of 46 and 6, and every sync paid for all of them.
    #[tokio::test]
    async fn seeding_the_address_cache_again_does_not_grow_it() {
        use bdk_wallet::KeychainKind::{External, Internal};
        let wallet = WalletBuilder::new(100_000).build();
        wallet.seed_address_cache(46, 6).await.unwrap();
        let first = (
            wallet.inner.next_derivation_index(External).await,
            wallet.inner.next_derivation_index(Internal).await,
        );

        for _ in 0..5 {
            wallet.seed_address_cache(46, 6).await.unwrap();
        }
        let after = (
            wallet.inner.next_derivation_index(External).await,
            wallet.inner.next_derivation_index(Internal).await,
        );

        assert_eq!(first, after);
    }

    #[tokio::test]
    async fn max_giveable_weight_tracks_the_selected_input_set() {
        let one_input = WalletBuilder::new(100_000).build();
        let two_inputs = WalletBuilder::new(100_000).with_num_utxos(2).build();

        let (_, _, one_weight) = one_input
            .max_giveable_with_fee_and_weight(crate::bitcoin::TxLock::script_size())
            .await
            .unwrap();
        let (_, _, two_weight) = two_inputs
            .max_giveable_with_fee_and_weight(crate::bitcoin::TxLock::script_size())
            .await
            .unwrap();

        assert!(two_weight > one_weight);
    }

    #[tokio::test]
    async fn max_giveable_reserves_the_lock_policy_fee() {
        let wallet = WalletBuilder::new(100_000).with_fees(1.58, 1000).build();
        let max_giveable = wallet
            .max_giveable(crate::bitcoin::TxLock::script_size())
            .await
            .unwrap();

        // 100,000 sats in, 123 vB lock at 1.58 sat/vB × 1.25 (243 sats),
        // not the inner wallet's flat 1,000-sat reserve.
        assert_eq!(max_giveable.to_sat(), 99_757);
    }

    #[tokio::test]
    async fn max_giveable_v5_reserves_the_v5_lock_weight() {
        let wallet = WalletBuilder::new(100_000).with_fees(1.0, 1000).build();
        let max_giveable = wallet.max_giveable_v5().await.unwrap();

        // 1 sat/vB × 1.25 on the v5 lock is 243 sats, below what an Eigenwallet maker
        // accepts, so the reserve is the 500-sat cross-client minimum (§457). It still
        // reserves exactly the lock fee: no change output, nothing left short.
        assert_eq!(max_giveable.to_sat(), 99_500);
    }

    /// hermes-v5's 6,132 sat/kvB: ceil(6.132) = 7, x1.25 = 8.75 -> 9 sat/vB, x 485 WU = 1,092 sats,
    /// the fee that left the accepted retry 592 sats short (0.00810592 needed of 0.00810000).
    #[test]
    fn the_seller_rate_forces_the_lock_fee_the_bob_machine_uses() {
        assert_eq!(v5_lock_fee_for_seller_rate(6_132).unwrap().to_sat(), 1_092);
        assert_eq!(v5_lock_fee_for_seller_rate(1_000).unwrap().to_sat(), 243);
    }

    #[tokio::test]
    async fn an_accepted_seller_rate_is_reserved_so_the_whole_balance_fits() {
        let wallet = WalletBuilder::new(810_000).with_fees(1.0, 1000).build();
        let plain = wallet.max_giveable_v5().await.unwrap();
        let reserved = wallet
            .max_giveable_v5_with_lock_fee_at_least(v5_lock_fee_for_seller_rate(6_132).unwrap())
            .await
            .unwrap();
        assert_eq!(plain.to_sat(), 809_500); // the amount that failed live
        assert_eq!(reserved.to_sat(), 810_000 - 1_092);
        assert_eq!(reserved + v5_lock_fee_for_seller_rate(6_132).unwrap(), Amount::from_sat(810_000));
    }

    /// Against floors computed by UNMODIFIED Eigenwallet 5.0.2 (protocol-v5/conformance/
    /// eigenwallet-5.0.2/fee-vectors.json, WORKLOG §461): for every advertised rate and amount, the
    /// lowest lock fee our buyer can pay (the 500-sat minimum, or what that rate forces) meets the
    /// Eigenwallet maker's floor. This is the check that failed live on 2026-11-11 (364 vs 500).
    #[test]
    fn our_lowest_lock_fee_meets_every_eigenwallet_maker_floor() {
        let vectors: serde_json::Value = serde_json::from_str(include_str!(
            "../../../protocol-v5/conformance/eigenwallet-5.0.2/fee-vectors.json"
        ))
        .unwrap();
        let floors = vectors["lock_floors"].as_array().unwrap();
        assert_eq!(floors.len(), 28);
        for case in floors {
            let rate = case["rate"].as_u64().unwrap();
            let floor = case["lock_floor"].as_u64().unwrap();
            let lowest = v5_lock_fee_for_seller_rate(rate)
                .unwrap()
                .max(V5_LOCK_FEE_MIN_FOR_ANY_MAKER)
                .to_sat();
            assert!(lowest >= floor, "{case}: our lowest lock fee {lowest} < their floor {floor}");
        }
    }

    #[test]
    fn a_quiet_chain_lock_is_lifted_to_what_any_maker_accepts() {
        assert_eq!(v5_lock_fee_for_any_maker(Amount::from_sat(243)).to_sat(), 500);
        assert_eq!(v5_lock_fee_for_any_maker(Amount::from_sat(364)).to_sat(), 500);
        // above the minimum our Dojo-based fee is used unchanged
        assert_eq!(v5_lock_fee_for_any_maker(Amount::from_sat(1_577)).to_sat(), 1_577);
    }

    /// Against Eigenwallet's ORIGINAL maker rule (5.0.2 `sanity_check_transaction_fee_floor`:
    /// fee >= estimate_fee(..) / 2, where estimate_fee has the flat 1,000-sat minimum), at the
    /// low rates where the live swap was rejected, and with small and large swap amounts.
    #[test]
    fn the_lifted_lock_fee_passes_the_eigenwallet_maker_floor() {
        let weight = crate::protocol_v5::core::bitcoin::TxLock::weight();
        let relay = FeeRate::from_sat_per_vb(1).unwrap();
        for sat_per_vb in [1u64, 2, 3] {
            let rate = FeeRate::from_sat_per_vb(sat_per_vb).unwrap();
            for amount in [9_636u64, 100_000, 1_500_000] {
                let estimate =
                    bitcoin_wallet::estimate_fee(weight, Some(Amount::from_sat(amount)), rate, relay).unwrap();
                let floor = Amount::from_sat(estimate.to_sat() / 2);
                let ours = rate
                    .checked_mul_by_weight(weight)
                    .unwrap();
                let lifted = v5_lock_fee_for_any_maker(ours);
                assert!(lifted >= floor, "{sat_per_vb} sat/vB, {amount} sats: {lifted} < {floor}");
            }
        }
        // the live rejection: 364 sats against an estimate of 1,000
        assert!(Amount::from_sat(364) < Amount::from_sat(1_000 / 2));
    }
    #[test]
    fn fee_target_policy_pins_the_stakes() {
        use crate::bitcoin::fee_targets;
        assert_eq!(fee_targets::FEE_TARGET_PRINCIPAL_CRITICAL, 1);
        assert_eq!(fee_targets::FEE_TARGET_TIME_CRITICAL, 2);
        assert_eq!(fee_targets::FEE_TARGET_TIMELIGHT_BOUND, 6);
        assert_eq!(crate::bitcoin::CANCEL_REFUND_FEE_RATE_MULTIPLIER, 2.0);
    }

    #[test]
    fn relay_floor_wins_when_it_exceeds_the_policy_cap() {
        let amount = Amount::from_sat(547);
        let fee_rate = Decimal::from(1);
        let relay_fee = FeeRate::from_sat_per_vb(1).unwrap();

        assert_eq!(
            estimate_fee_vsize(140, amount, fee_rate, relay_fee)
                .unwrap()
                .to_sat(),
            175
        );
        assert_eq!(
            estimate_fee_weight(400, amount, fee_rate, relay_fee)
                .unwrap()
                .to_sat(),
            125
        );
    }

    #[test]
    fn given_100k_sat_and_1k_sats_per_vb_fees_should_hit_relative_max() {
        let fee = estimate_fee_weight(
            400,
            Amount::from_sat(100_000),
            Decimal::from(1_000),
            FeeRate::from_sat_per_vb(1).unwrap(),
        )
        .unwrap();
        assert_eq!(fee, Amount::from_sat(20_000));
    }

    #[test]
    fn given_large_transfer_and_high_fee_rate_fees_hit_the_absolute_max() {
        let fee = estimate_fee_weight(
            400,
            Amount::from_sat(10_000_000),
            Decimal::from(2_000),
            FeeRate::from_sat_per_vb(1).unwrap(),
        )
        .unwrap();
        assert_eq!(fee, Amount::from_sat(100_000));
    }

    #[tokio::test]
    async fn per_tx_fee_estimate_passes_the_requested_target_to_the_estimator() {
        let fee_rate = WalletBuilder::target_aware_fee_rate();
        let requested_targets = fee_rate.requested_targets.clone();
        let wallet = WalletBuilder::new(50_000)
            .build_with_fee_strategy(fee_rate);

        let amount = Amount::from_sat(100_000);
        wallet
            .estimate_fee_vsize_at(
                crate::bitcoin::TxCancel::vsize(),
                amount,
                crate::bitcoin::fee_targets::FEE_TARGET_PRINCIPAL_CRITICAL,
            )
            .await
            .unwrap();
        wallet
            .estimate_fee_vsize_at(
                crate::bitcoin::TxRefund::vsize(),
                amount,
                crate::bitcoin::fee_targets::FEE_TARGET_TIMELIGHT_BOUND,
            )
            .await
            .unwrap();

        assert_eq!(*requested_targets.lock().unwrap(), vec![1, 6]);
    }

    #[tokio::test]
    async fn principal_critical_rate_pays_more_than_timelock_bound_rate() {
        let wallet = WalletBuilder::new(50_000)
            .build_with_fee_strategy(WalletBuilder::target_aware_fee_rate());

        let amount = Amount::from_sat(100_000);
        let principal_critical = wallet
            .estimate_fee_vsize_at(crate::bitcoin::TxCancel::vsize(), amount, 1)
            .await
            .unwrap()
            .to_sat();
        let timelock_bound = wallet
            .estimate_fee_vsize_at(crate::bitcoin::TxRefund::vsize(), amount, 6)
            .await
            .unwrap()
            .to_sat();

        assert_eq!(principal_critical, 3_000);
        assert_eq!(timelock_bound, 420);
        assert!(principal_critical > timelock_bound);
    }

    #[tokio::test]
    async fn fee_estimates_grow_strictly_with_confirmation_urgency() {
        let wallet = WalletBuilder::new(50_000)
            .build_with_fee_strategy(WalletBuilder::target_aware_fee_rate());

        let amount = Amount::from_sat(10_000_000);
        let fee_at = async |target: usize| {
            wallet
                .estimate_fee_vsize_at(2_000, amount, target)
                .await
                .unwrap()
                .to_sat()
        };

        assert_eq!(
            (
                fee_at(1).await,
                fee_at(2).await,
                fee_at(6).await,
                fee_at(9).await
            ),
            (40_000, 20_000, 6_000, 2_500)
        );
    }

    #[tokio::test]
    async fn cancel_refund_rate_buffer_doubles_the_estimate() {
        let wallet = WalletBuilder::new(50_000).with_fees(10.0, 1000).build();
        let amount = Amount::from_sat(1_000_000);

        let plain = wallet
            .estimate_fee_vsize(crate::bitcoin::TxCancel::vsize(), amount)
            .await
            .unwrap();
        let buffered = wallet
            .estimate_fee_vsize_with_rate_multiplier(
                crate::bitcoin::TxCancel::vsize(),
                amount,
                crate::bitcoin::CANCEL_REFUND_FEE_RATE_MULTIPLIER,
            )
            .await
            .unwrap();

        assert_eq!(plain.to_sat(), 1_500);
        assert_eq!(buffered.to_sat(), 3_000);
    }

    #[tokio::test]
    async fn cancel_refund_rate_buffer_is_absorbed_by_the_relay_floor() {
        let wallet = WalletBuilder::new(50_000).with_fees(0.5, 1000).build();
        let amount = Amount::from_sat(1_000_000);

        let buffered = wallet
            .estimate_fee_vsize_with_rate_multiplier(
                crate::bitcoin::TxCancel::vsize(),
                amount,
                crate::bitcoin::CANCEL_REFUND_FEE_RATE_MULTIPLIER,
            )
            .await
            .unwrap();

        assert_eq!(buffered.to_sat(), 188);
    }

    #[tokio::test]
    async fn cancel_refund_rate_buffer_is_clamped_by_the_relative_cap() {
        let wallet = WalletBuilder::new(50_000).with_fees(20.0, 1000).build();
        let amount = Amount::from_sat(10_000);

        let buffered = wallet
            .estimate_fee_vsize_with_rate_multiplier(
                crate::bitcoin::TxCancel::vsize(),
                amount,
                crate::bitcoin::CANCEL_REFUND_FEE_RATE_MULTIPLIER,
            )
            .await
            .unwrap();

        assert_eq!(buffered.to_sat(), 2_000);
    }

    #[tokio::test]
    async fn fee_policy_matrix_matches_the_reference_rates() {
        let amount = Amount::from_sat(100_000);

        for (rate, expected) in [
            (5.0, [769, 875, 1_500, 1_400, 1_400]),
            (10.0, [1_538, 1_750, 3_000, 2_800, 2_800]),
            (20.0, [3_075, 3_500, 6_000, 5_600, 5_600]),
        ] {
            let wallet = WalletBuilder::new(50_000).with_fees(rate, 1000).build();

            let fees = [
                wallet.estimate_lock_fee(amount).await.unwrap().to_sat(),
                wallet
                    .estimate_fee_vsize_with_rate_multiplier(
                        crate::bitcoin::TxRedeem::vsize(),
                        amount,
                        crate::bitcoin::LOCK_REDEEM_FEE_RATE_MULTIPLIER,
                    )
                    .await
                    .unwrap()
                    .to_sat(),
                wallet
                    .estimate_fee_vsize_with_rate_multiplier(
                        crate::bitcoin::TxCancel::vsize(),
                        amount,
                        crate::bitcoin::CANCEL_REFUND_FEE_RATE_MULTIPLIER,
                    )
                    .await
                    .unwrap()
                    .to_sat(),
                wallet
                    .estimate_fee_vsize_with_rate_multiplier(
                        crate::bitcoin::TxRefund::vsize(),
                        amount,
                        crate::bitcoin::CANCEL_REFUND_FEE_RATE_MULTIPLIER,
                    )
                    .await
                    .unwrap()
                    .to_sat(),
                wallet
                    .estimate_fee_vsize_with_rate_multiplier(
                        crate::bitcoin::TxPunish::vsize(),
                        amount,
                        crate::bitcoin::PUNISH_FEE_RATE_MULTIPLIER,
                    )
                    .await
                    .unwrap()
                    .to_sat(),
            ];

            assert_eq!(fees, expected);
        }
    }

    #[tokio::test]
    async fn fees_never_exceed_the_amount_being_swapped() {
        for amount_sats in [547, 100_000] {
            let amount = Amount::from_sat(amount_sats);

            for rate in [1.0, 20.0, 200.0, 10_000.0] {
                let wallet = WalletBuilder::new(50_000).with_fees(rate, 1000).build();

                for fee in [
                    wallet.estimate_lock_fee(amount).await.unwrap(),
                    wallet
                        .estimate_fee_vsize(crate::bitcoin::TxRedeem::vsize(), amount)
                        .await
                        .unwrap(),
                    wallet
                        .estimate_fee_vsize_with_rate_multiplier(
                            crate::bitcoin::TxCancel::vsize(),
                            amount,
                            crate::bitcoin::CANCEL_REFUND_FEE_RATE_MULTIPLIER,
                        )
                        .await
                        .unwrap(),
                ] {
                    assert!(
                        fee < amount,
                        "fee {fee} must stay below swap amount {amount} at {rate} sat/vB"
                    );
                }
            }
        }
    }

}
