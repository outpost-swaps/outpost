use crate::coin_selection;
use crate::primitives::{Confirmed, EstimateFeeRate, ScriptStatus, Subscription, Watchable};
use crate::{
    BitcoinWallet, BlockHeight, DojoFeeEstimator, PsbtPrevoutVerificationError, RpcErrorCode,
    VerifyPsbtPrevouts, bitcoin_address, parse_rpc_error_code,
};
use anyhow::{Context, Result, anyhow, bail};
use bdk_chain::CheckPoint;
use bdk_chain::spk_client::{SyncRequest, SyncRequestBuilder};
use bdk_electrum::electrum_client::{ElectrumApi, GetHistoryRes};

use bdk_wallet::KeychainKind;
use bdk_wallet::WalletPersister;
use bdk_wallet::bitcoin::FeeRate;
use bdk_wallet::bitcoin::Network;
use bdk_wallet::descriptor::DescriptorError;
use bdk_wallet::export::FullyNodedExport;
use bdk_wallet::keys::DerivableKey;
use bdk_wallet::miniscript::Segwitv0;
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::template::{DescriptorTemplate, DescriptorTemplateOut, P2Wpkh};
use bdk_wallet::{Balance, PersistedWallet};
use bitcoin::bip32::{ChildNumber, DerivationPath, Xpriv};
use bitcoin::transaction::{InputWeightPrediction, predict_weight};
use bitcoin::{Address, Amount, Transaction, Txid, psbt::Psbt as PartiallySignedTransaction};
use bitcoin::{Psbt, ScriptBuf, Weight};
use derive_builder::Builder;
use electrum_pool::{ElectrumBalancer, ElectrumBalancerConfig};
use moka;
use rust_decimal::Decimal;
use rust_decimal::prelude::*;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt::Debug;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as SyncMutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use std::time::Instant;
use sync_ext::{CumulativeProgressHandle, InnerSyncCallback, SyncCallbackExt};
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::RwLock as TokioRwLock;
use tokio::sync::watch;
use tracing::{Instrument, debug_span};

pub type TauriHandle = Option<Arc<dyn BitcoinTauriHandle>>;
pub trait BitcoinTauriHandle: Send + Sync {
    /// let progress_handle = tauri_handle.new_background_process_with_initial_progress(
    ///     TauriBackgroundProgress::FullScanningBitcoinWallet,
    ///     TauriBitcoinFullScanProgress::Unknown,
    /// );
    fn start_full_scan(&self) -> Arc<dyn BitcoinTauriBackgroundTask>;

    /// let background_process_handle = self
    ///     .tauri_handle
    ///     .new_background_process_with_initial_progress(
    ///         TauriBackgroundProgress::SyncingBitcoinWallet,
    ///         TauriBitcoinSyncProgress::Unknown,
    ///     );
    fn start_sync(&self) -> Arc<dyn BitcoinTauriBackgroundTask>;
}
pub trait BitcoinTauriBackgroundTask: Send + Sync {
    /// progress_handle_clone.update(TauriBitcoinFullScanProgress::Known {
    ///     current_index: consumed,
    ///     assumed_total: total,
    /// });
    /// or
    /// background_process_handle_clone
    ///     .update(TauriBitcoinSyncProgress::Known { consumed, total });
    fn update(&self, consumed: u64, total: u64);

    /// background_process_handle.finish();
    fn finish(&self);
}

pub trait BitcoinWalletSeed {
    fn derive_extended_private_key(&self, network: bitcoin::Network) -> Result<Xpriv>;

    /// Same as `derive_extended_private_key`, but using the legacy BDK API.
    ///
    /// This is only used for the migration path from the old wallet format to the new one.
    fn derive_extended_private_key_legacy(
        &self,
        network: bdk::bitcoin::Network,
    ) -> Result<bdk::bitcoin::util::bip32::ExtendedPrivKey>;
}

/// We allow transaction fees of up to 20% of the transferred amount to ensure
/// that lock transactions can always be published, even when fees are high.
const TWENTY_PERCENT: Decimal = Decimal::from_parts(20, 0, 0, false, 2);
pub const MAX_RELATIVE_TX_FEE: Decimal = TWENTY_PERCENT;
pub const MAX_ABSOLUTE_TX_FEE: Amount = Amount::from_sat(100_000);
pub const MIN_ABSOLUTE_TX_FEE_SATS: u64 = 1000;
pub const MIN_ABSOLUTE_TX_FEE: Amount = Amount::from_sat(MIN_ABSOLUTE_TX_FEE_SATS);
pub const DUST_AMOUNT: Amount = Amount::from_sat(546);
const DOJO_BROADCAST_TIMEOUT: Duration = Duration::from_secs(15);
const DOJO_BROADCAST_ATTEMPTS: u32 = 2;
const DOJO_BROADCAST_RETRY_DELAY: Duration = Duration::from_secs(2);

// Samourai-compatible account layout. These account numbers are part of the
// user's wallet derivation contract and must never change silently.
const BAD_BANK: u32 = 2_147_483_644;
const SWAPS_DEPOSIT_ACCOUNT: u32 = BAD_BANK - 1;
const SWAPS_REFUNDS_ACCOUNT: u32 = BAD_BANK - 2;
const ASB_ACCOUNT: u32 = BAD_BANK - 3;

fn legacy_wallet_network(network: Network) -> Result<bdk::bitcoin::Network> {
    Ok(match network {
        Network::Bitcoin => bdk::bitcoin::Network::Bitcoin,
        // The pre-1.0 wallet only knew testnet3, but its key derivation and
        // address prefixes are identical on testnet4.
        Network::Testnet | Network::Testnet4 => bdk::bitcoin::Network::Testnet,
        _ => bail!("Unsupported network: {}", network),
    })
}

/// The internal swap-wallet account to derive.
///
/// This mirrors the note that is handed to the paired Samourai/Ashigaru
/// wallet, so the swap wallet spends from the account the user can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletAccount {
    Deposit,
    Refund,
    Asb,
}

impl WalletAccount {
    pub const fn account_number(self) -> u32 {
        match self {
            Self::Deposit => SWAPS_DEPOSIT_ACCOUNT,
            Self::Refund => SWAPS_REFUNDS_ACCOUNT,
            Self::Asb => ASB_ACCOUNT,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Deposit => "swaps-deposit",
            Self::Refund => "swaps-refunds",
            Self::Asb => "asb",
        }
    }
}

/// BIP84 template that derives the Samourai atomic-swap account instead of
/// account zero.
#[derive(Debug, Clone)]
pub struct SamouraiBip84<K: DerivableKey<Segwitv0>>(pub K, pub KeychainKind, pub WalletAccount);

impl<K: DerivableKey<Segwitv0>> DescriptorTemplate for SamouraiBip84<K> {
    fn build(self, network: Network) -> Result<DescriptorTemplateOut, DescriptorError> {
        let coin_type = match network {
            Network::Bitcoin => 0,
            _ => 1,
        };
        let keychain = match self.1 {
            KeychainKind::External => 0,
            KeychainKind::Internal => 1,
        };
        let path = DerivationPath::from(vec![
            ChildNumber::from_hardened_idx(84).expect("84 is a valid hardened index"),
            ChildNumber::from_hardened_idx(coin_type).expect("coin type is a valid hardened index"),
            ChildNumber::from_hardened_idx(self.2.account_number())
                .expect("account number is a valid hardened index"),
            ChildNumber::from_normal_idx(keychain).expect("keychain is a valid normal index"),
        ]);

        P2Wpkh((self.0, path)).build(network)
    }
}

/// This is our wrapper around a bdk wallet and a corresponding
/// bdk electrum client.
/// It unifies all the functionality we need when interacting
/// with the bitcoin network.
///
/// This wallet is generic over the persister, which may be a
/// rusqlite connection, or an in-memory database, or something else.
pub struct Wallet<Persister = Connection, C = Client> {
    /// The wallet, which is persisted to the disk.
    wallet: Arc<TokioMutex<PersistedWallet<Persister>>>,
    /// The database connection used to persist the wallet.
    persister: Arc<TokioMutex<Persister>>,
    /// The electrum client.
    electrum_client: Arc<C>,
    /// The cached fee estimator for the electrum client.
    cached_electrum_fee_estimator: Arc<CachedFeeEstimator<C>>,
    /// The cached fee estimator for the mempool client.
    cached_mempool_fee_estimator: Arc<Option<CachedFeeEstimator<mempool_client::MempoolClient>>>,
    /// The network this wallet is on.
    network: Network,
    /// The number of confirmations (blocks) we require for a transaction
    /// to be considered final.
    ///
    /// Usually set to 1.
    finality_confirmations: u32,
    /// We want our transactions to be confirmed after this many blocks
    /// (used for fee estimation).
    target_block: u32,
    /// The root extended private key the wallet descriptors derive from.
    xprivkey: Xpriv,
    /// The paired Dojo's fee and chain view. Mutable because the GUI/ASB
    /// supplies decrypted credentials after the wallet is constructed.
    dojo: Arc<TokioRwLock<Option<Arc<DojoFeeEstimator>>>>,
    /// Fail rather than falling back to Electrum if Dojo fee estimation fails.
    require_dojo: Arc<AtomicBool>,
    /// The Tauri handle
    tauri_handle: TauriHandle,
    /// Limits which revealed scripts a sync checks (see [`SyncWindow`]).
    sync_window: Arc<SyncWindow>,
}

/// How many scripts past the highest used (or externally known) index a sync
/// checks on each keychain.
const SYNC_WINDOW_GAP: u32 = 20;

/// Limits a sync to the scripts that can actually hold funds.
///
/// A wallet database only ever grows its set of revealed scripts, and every sync
/// asks the server about all of them. An old deposit wallet had 676 revealed
/// scripts per keychain for real indices of 46 and 6, which made each sync take
/// 70-90 s over Tor. Once the caller has told us where the account really is
/// (`seed_address_cache`), a sync only checks indices below
/// `max(highest used + 1, known index) + SYNC_WINDOW_GAP` on each keychain.
/// Addresses are handed out with `next_unused_address`, so any address a user
/// can be given stays inside that window. A wallet that was never seeded keeps
/// checking every revealed script.
#[derive(Debug, Default)]
pub struct SyncWindow {
    seeded: AtomicBool,
    receive_floor: AtomicU32,
    change_floor: AtomicU32,
}

impl SyncWindow {
    fn set(&self, receive_floor: u32, change_floor: u32) {
        self.receive_floor.store(receive_floor, Ordering::SeqCst);
        self.change_floor.store(change_floor, Ordering::SeqCst);
        self.seeded.store(true, Ordering::SeqCst);
    }

    /// Exclusive upper index per keychain, or `None` while nothing limits the sync.
    fn floors(&self) -> Option<(u32, u32)> {
        self.seeded.load(Ordering::SeqCst).then(|| {
            (
                self.receive_floor.load(Ordering::SeqCst),
                self.change_floor.load(Ordering::SeqCst),
            )
        })
    }
}

/// Exclusive upper script index a sync checks on one keychain.
fn sync_window_end(last_used: Option<u32>, floor: u32) -> u32 {
    last_used
        .map(|index| index.saturating_add(1))
        .unwrap_or(0)
        .max(floor)
        .saturating_add(SYNC_WINDOW_GAP)
}

impl<Persister, C> Clone for Wallet<Persister, C> {
    fn clone(&self) -> Self {
        Self {
            wallet: Arc::clone(&self.wallet),
            persister: Arc::clone(&self.persister),
            electrum_client: Arc::clone(&self.electrum_client),
            cached_electrum_fee_estimator: Arc::clone(&self.cached_electrum_fee_estimator),
            cached_mempool_fee_estimator: Arc::clone(&self.cached_mempool_fee_estimator),
            network: self.network,
            finality_confirmations: self.finality_confirmations,
            target_block: self.target_block,
            xprivkey: self.xprivkey,
            dojo: Arc::clone(&self.dojo),
            require_dojo: Arc::clone(&self.require_dojo),
            tauri_handle: self.tauri_handle.clone(),
            sync_window: Arc::clone(&self.sync_window),
        }
    }
}

/// The lowest mainnet block-header difficulty accepted as proof of work when
/// checking that a transaction was mined (audit H-4).
///
/// A merkle proof only shows a transaction is in *some* header. Without a floor
/// a lying server could make up a header with trivial work. Faking one at this
/// difficulty costs about a third of a real block's work, which is far more than
/// stalling or destroying one swap is worth. Mainnet difficulty has been above
/// 50T since 2023; if it ever fell below this, confirmations would stop being
/// accepted (swaps wait) rather than being accepted unproven.
pub const MAINNET_MIN_HEADER_DIFFICULTY: u128 = 50_000_000_000_000;

/// How many proven inclusions [`Client`] remembers before starting over.
const MAX_VERIFIED_INCLUSIONS: usize = 10_000;

/// The header difficulty floor for `network`. Only mainnet has one: test
/// networks mine minimum-difficulty blocks, so any floor would refuse real
/// blocks there. On those networks the merkle proof and the header's own proof
/// of work are still checked.
pub fn min_header_difficulty(network: Network) -> Option<u128> {
    match network {
        Network::Bitcoin => Some(MAINNET_MIN_HEADER_DIFFICULTY),
        _ => None,
    }
}

/// The outcome of checking the servers' proofs that a transaction was mined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InclusionVerdict {
    /// At least one server gave a merkle proof that checks out against a
    /// header with valid proof of work.
    Proven,
    /// Servers answered, but none proved the transaction is in that block.
    Refuted(String),
}

/// Check that `proof` ties `txid` to `header`, and that the header carries
/// valid proof of work of at least `min_difficulty` (audit H-4).
pub fn verify_inclusion(
    txid: &Txid,
    proof: &bdk_electrum::electrum_client::GetMerkleRes,
    header: &bitcoin::block::Header,
    min_difficulty: Option<u128>,
) -> Result<()> {
    if !bdk_electrum::electrum_client::utils::validate_merkle_proof(
        txid,
        &header.merkle_root,
        proof,
    ) {
        bail!(
            "the merkle proof for {} does not lead to the merkle root of block {}",
            txid,
            header.block_hash()
        );
    }

    header.validate_pow(header.target()).map_err(|error| {
        anyhow!(
            "block {} does not meet its own proof-of-work target: {}",
            header.block_hash(),
            error
        )
    })?;

    if let Some(min_difficulty) = min_difficulty {
        let difficulty = header.difficulty(Network::Bitcoin);
        if difficulty < min_difficulty {
            bail!(
                "block {} has difficulty {}, below the accepted minimum of {}",
                header.block_hash(),
                difficulty,
                min_difficulty
            );
        }
    }

    Ok(())
}

/// Decide from every server's (proof, header) answer whether `txid` is proven
/// mined. One valid proof is enough. A server that answers with an RPC error
/// (for example "transaction not in block") counts as an answer; only when no
/// server answered at all is the result an error, so the caller retries.
pub fn judge_inclusion_proofs(
    txid: &Txid,
    results: Vec<
        Result<
            (bdk_electrum::electrum_client::GetMerkleRes, bitcoin::block::Header),
            bdk_electrum::electrum_client::Error,
        >,
    >,
    min_difficulty: Option<u128>,
) -> Result<InclusionVerdict> {
    let mut reasons = Vec::new();
    let mut unreachable = Vec::new();

    for result in results {
        match result {
            Ok((proof, header)) => match verify_inclusion(txid, &proof, &header, min_difficulty) {
                Ok(()) => return Ok(InclusionVerdict::Proven),
                Err(error) => reasons.push(format!("{:#}", error)),
            },
            Err(bdk_electrum::electrum_client::Error::Protocol(answer)) => {
                reasons.push(format!("server answered: {}", answer))
            }
            Err(error) => unreachable.push(error),
        }
    }

    if !reasons.is_empty() {
        return Ok(InclusionVerdict::Refuted(reasons.join("; ")));
    }

    Err(anyhow::Error::from(electrum_pool::MultiError::new(
        unreachable,
        format!("No Electrum server answered the merkle proof request for {}", txid),
    )))
}

/// The first transaction among the servers' answers that really has `txid`.
/// A server answering with some other transaction is ignored (audit H-4).
fn matching_transaction<'a, E>(
    txid: &Txid,
    results: &'a [std::result::Result<Option<Transaction>, E>],
) -> Option<&'a Transaction> {
    results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .filter_map(|response| response.as_ref())
        .find(|tx| {
            let matches = tx.compute_txid() == *txid;
            if !matches {
                tracing::warn!(
                    requested = %txid,
                    returned = %tx.compute_txid(),
                    "An Electrum server answered with a different transaction; ignoring it"
                );
            }
            matches
        })
}

/// This is our wrapper around a bdk electrum client.
#[derive(Clone)]
pub struct Client {
    /// The underlying electrum balancer for load balancing across multiple servers.
    inner: Arc<ElectrumBalancer>,
    /// The history of transactions for each script.
    script_history: Arc<TokioRwLock<BTreeMap<ScriptBuf, Vec<GetHistoryRes>>>>,
    /// The status-update channels we poll, keyed by the watched transaction and script.
    subscriptions: Arc<TokioMutex<HashMap<(Txid, ScriptBuf), watch::Sender<ScriptStatus>>>>,
    /// The time of the last sync.
    last_sync: Arc<SyncMutex<Instant>>,
    /// How often we sync with the server.
    sync_interval: Duration,
    /// How long a subscription is kept alive after its last receiver is dropped.
    subscription_idle_timeout: Duration,
    /// The height of the latest block we know about.
    latest_block_height: Arc<SyncMutex<BlockHeight>>,
    /// The lowest block-header difficulty accepted as proof that a transaction
    /// was mined (audit H-4). Set on mainnet only: test networks allow
    /// minimum-difficulty blocks, so a floor there would refuse real blocks.
    min_header_difficulty: Option<u128>,
    /// Inclusions already proven, as (txid, block height), so a confirmed
    /// transaction is checked once rather than on every poll.
    verified_inclusions: Arc<SyncMutex<std::collections::HashSet<(Txid, u32)>>>,
}

const DEFAULT_SUBSCRIPTION_IDLE_TIMEOUT: Duration = Duration::from_secs(4 * 60);

/// Holds the configuration parameters for creating a Bitcoin wallet.
/// The actual Wallet<Connection> will be constructed from this configuration.
#[derive(Builder, Clone)]
#[builder(
    name = "WalletBuilder",
    pattern = "owned",
    setter(into, strip_option),
    build_fn(
        name = "validate_config",
        private,
        error = "derive_builder::UninitializedFieldError"
    ),
    derive(Clone)
)]
pub struct WalletConfig<Seed: BitcoinWalletSeed> {
    seed: Seed,
    network: Network,
    account: WalletAccount,
    electrum_rpc_urls: Vec<String>,
    persister: PersisterConfig,
    finality_confirmations: u32,
    target_block: u32,
    sync_interval: Duration,
    #[builder(default = "DEFAULT_SUBSCRIPTION_IDLE_TIMEOUT")]
    subscription_idle_timeout: Duration,
    #[builder(default)]
    tauri_handle: TauriHandle,
    #[builder(default)]
    use_mempool_space_fee_estimation: bool,
    #[builder(default)]
    dojo: Option<Arc<DojoFeeEstimator>>,
    #[builder(default)]
    require_dojo: bool,
    #[builder(default)]
    socks5_proxy: Option<String>,
}

impl<Seed: BitcoinWalletSeed> WalletBuilder<Seed> {
    /// Asynchronously builds the `Wallet<Connection>` using the configured parameters.
    /// This method contains the core logic for wallet initialization, including
    /// database setup, key derivation, and potential migration from older wallet formats.
    pub async fn build(self) -> Result<Wallet<Connection, Client>> {
        let config = self
            .validate_config()
            .map_err(|e| anyhow!("Builder validation failed: {e}"))?;

        let mut client = Client::new(
            &config.electrum_rpc_urls,
            config.sync_interval,
            config.socks5_proxy.clone(),
            config.network,
        )
        .await
        .context("Failed to create Electrum client")?;
        client.subscription_idle_timeout = config.subscription_idle_timeout;

        if let Some(dojo) = config.dojo.as_ref() {
            dojo.spawn_warm_up();
        }

        match &config.persister {
            PersisterConfig::SqliteFile { data_dir } => {
                let xprivkey = config
                    .seed
                    .derive_extended_private_key(config.network)
                    .context("Failed to derive extended private key for file wallet")?;

                let wallet_parent_dir = data_dir.join(Wallet::<Connection>::WALLET_PARENT_DIR_NAME);
                let wallet_dir = wallet_parent_dir.join(Wallet::<Connection>::WALLET_DIR_NAME);
                let wallet_path = wallet_dir.join(Wallet::<Connection>::WALLET_FILE_NAME);
                let wallet_exists = wallet_path.exists();

                tokio::fs::create_dir_all(&wallet_dir)
                    .await
                    .context("Failed to create wallet directory")?;

                let open_connection = || -> Result<Connection> {
                    Connection::open(&wallet_path).context(format!(
                        "Failed to open SQLite database at {:?}",
                        wallet_path
                    ))
                };

                if wallet_exists {
                    let connection = open_connection()?;

                    Wallet::create_existing(
                        xprivkey,
                        config.network,
                        config.account,
                        config.dojo.clone(),
                        config.require_dojo,
                        client,
                        connection,
                        config.finality_confirmations,
                        config.target_block,
                        config.tauri_handle.clone(),
                        config.use_mempool_space_fee_estimation,
                    )
                    .await
                    .context("Failed to load existing wallet")
                } else {
                    let old_wallet_export = Wallet::<Connection>::get_pre_1_0_bdk_wallet_export(
                        data_dir,
                        config.network,
                        config.account,
                        &config.seed,
                    )
                    .await
                    .context("Failed to get pre-1.0.0 BDK wallet export for migration")?;

                    Wallet::create_new(
                        xprivkey,
                        config.network,
                        config.account,
                        config.dojo.clone(),
                        config.require_dojo,
                        client,
                        open_connection,
                        config.finality_confirmations,
                        config.target_block,
                        old_wallet_export,
                        config.tauri_handle.clone(),
                        config.use_mempool_space_fee_estimation,
                    )
                    .await
                    .context("Failed to create new wallet")
                }
            }
            PersisterConfig::InMemorySqlite => {
                let xprivkey = config
                    .seed
                    .derive_extended_private_key(config.network)
                    .context("Failed to derive extended private key for in-memory wallet")?;

                let persister = Connection::open_in_memory()
                    .context("Failed to open in-memory SQLite database")?;

                Wallet::create_new::<Connection>(
                    xprivkey,
                    config.network,
                    config.account,
                    config.dojo.clone(),
                    config.require_dojo,
                    client,
                    move || Ok(persister),
                    config.finality_confirmations,
                    config.target_block,
                    None,
                    config.tauri_handle.clone(),
                    config.use_mempool_space_fee_estimation,
                )
                .await
                .context("Failed to create new in-memory wallet")
            }
        }
    }
}

/// Configuration for how the wallet should be persisted.
#[derive(Debug, Clone)]
pub enum PersisterConfig {
    SqliteFile { data_dir: PathBuf },
    InMemorySqlite,
}

/// A caching wrapper around EstimateFeeRate implementations.
///
/// Uses Moka cache with TTL (Time To Live) expiration for both fee rate estimates
/// and minimum relay fees to reduce the frequency of network calls to Electrum and mempool.space APIs.
#[derive(Clone)]
pub struct CachedFeeEstimator<T> {
    inner: T,
    fee_cache: Arc<moka::future::Cache<u32, FeeRate>>,
    min_relay_cache: Arc<moka::future::Cache<(), FeeRate>>,
}

impl<T> CachedFeeEstimator<T> {
    /// Cache duration for fee estimates (2 minutes)
    const CACHE_DURATION: Duration = Duration::from_secs(120);
    /// Maximum number of cached fee rate entries (different target blocks)
    const MAX_CACHE_SIZE: u64 = 10;

    /// Create a new caching wrapper around an EstimateFeeRate implementation.
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            fee_cache: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(Self::MAX_CACHE_SIZE)
                    .time_to_live(Self::CACHE_DURATION)
                    .build(),
            ),
            min_relay_cache: Arc::new(
                moka::future::Cache::builder()
                    .max_capacity(1) // Only one min relay fee value
                    .time_to_live(Self::CACHE_DURATION)
                    .build(),
            ),
        }
    }
}

impl<T: EstimateFeeRate + Send + Sync> EstimateFeeRate for CachedFeeEstimator<T> {
    async fn estimate_feerate(&self, target_block: u32) -> Result<FeeRate> {
        // Check cache first
        if let Some(cached_rate) = self.fee_cache.get(&target_block).await {
            return Ok(cached_rate);
        }

        // If not in cache, fetch from underlying estimator
        let fee_rate = self.inner.estimate_feerate(target_block).await?;

        // Store in cache
        self.fee_cache.insert(target_block, fee_rate).await;

        Ok(fee_rate)
    }

    async fn min_relay_fee(&self) -> Result<FeeRate> {
        // Check cache first
        if let Some(cached_rate) = self.min_relay_cache.get(&()).await {
            return Ok(cached_rate);
        }

        // If not in cache, fetch from underlying estimator
        let min_relay_fee = self.inner.min_relay_fee().await?;

        // Store in cache
        self.min_relay_cache.insert((), min_relay_fee).await;

        Ok(min_relay_fee)
    }
}

impl<T> std::ops::Deref for CachedFeeEstimator<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Wallet {
    /// If this many consequent addresses are unused, we stop the full scan.
    /// On old wallets we used to generate a ton of unused addresses
    /// which results in us having a bunch of large gaps in the SPKs
    const SCAN_STOP_GAP: u32 = 500;
    /// The batch size for syncing
    const SCAN_BATCH_SIZE: u32 = 32;
    /// The number of maximum chunks to use when syncing
    const SCAN_CHUNKS: u32 = 5;

    /// Maximum time we are willing to spend retrying a wallet sync
    const SYNC_MAX_ELAPSED_TIME: Duration = Duration::from_secs(180);

    const WALLET_PARENT_DIR_NAME: &str = "wallet";
    const WALLET_DIR_NAME: &str = "wallet-post-bdk-1.0";
    const WALLET_FILE_NAME: &str = "wallet-db.sqlite";

    async fn get_pre_1_0_bdk_wallet_export(
        data_dir: impl AsRef<Path>,
        network: Network,
        account: WalletAccount,
        seed: &impl BitcoinWalletSeed,
    ) -> Result<Option<pre_1_0_0_bdk::Export>> {
        // Construct the directory in which the old (<1.0 bdk) wallet was stored
        let wallet_parent_dir = data_dir.as_ref().join(Self::WALLET_PARENT_DIR_NAME);
        let pre_bdk_1_0_wallet_dir = wallet_parent_dir.join(pre_1_0_0_bdk::WALLET);
        let pre_bdk_1_0_wallet_exists = pre_bdk_1_0_wallet_dir.exists();

        if pre_bdk_1_0_wallet_exists {
            tracing::info!("Found old Bitcoin wallet (pre 1.0 bdk). Migrating...");

            // We need to support the legacy wallet format for the migration path.
            // We need to convert the network to the legacy BDK network type.
            let legacy_network = legacy_wallet_network(network)?;

            let xprivkey = seed.derive_extended_private_key_legacy(legacy_network)?;
            let old_wallet = pre_1_0_0_bdk::OldWallet::new(
                &pre_bdk_1_0_wallet_dir,
                xprivkey,
                legacy_network,
                account,
            )
            .await?;

            let export = old_wallet.export("old-wallet").await?;

            tracing::debug!(
                external_index=%export.external_derivation_index,
                internal_index=%export.internal_derivation_index,
                "Constructed export of old Bitcoin wallet (pre 1.0 bdk) for migration"
            );

            Ok(Some(export))
        } else {
            Ok(None)
        }
    }

    /// Create a new wallet, persisted to a sqlite database.
    /// This is a private API so we allow too many arguments.
    #[allow(clippy::too_many_arguments)]
    pub async fn with_sqlite(
        seed: &impl BitcoinWalletSeed,
        network: Network,
        account: WalletAccount,
        dojo: Option<Arc<DojoFeeEstimator>>,
        require_dojo: bool,
        electrum_rpc_urls: &[String],
        socks5_proxy: Option<String>,
        data_dir: impl AsRef<Path>,
        finality_confirmations: u32,
        target_block: u32,
        sync_interval: Duration,
        tauri_handle: TauriHandle,
    ) -> Result<Wallet<bdk_wallet::rusqlite::Connection, Client>> {
        // Construct the private key, directory and wallet file for the new (>= 1.0.0) bdk wallet
        let xprivkey = seed.derive_extended_private_key(network)?;
        let wallet_dir = data_dir
            .as_ref()
            .join(Self::WALLET_PARENT_DIR_NAME)
            .join(Self::WALLET_DIR_NAME);
        let wallet_path = wallet_dir.join(Self::WALLET_FILE_NAME);
        let wallet_exists = wallet_path.exists();

        // Connect to the electrum server.
        let client =
            Client::new(electrum_rpc_urls, sync_interval, socks5_proxy, network).await?;

        // Make sure the wallet directory exists.
        tokio::fs::create_dir_all(&wallet_dir).await?;

        let connection =
            || Connection::open(&wallet_path).context("Failed to open SQLite database");

        // If the new Bitcoin wallet (> 1.0.0 bdk) already exists, we open it
        if wallet_exists {
            Self::create_existing(
                xprivkey,
                network,
                account,
                dojo.clone(),
                require_dojo,
                client,
                connection()?,
                finality_confirmations,
                target_block,
                tauri_handle,
                false, // Outpost never calls clearnet fee APIs
            )
            .await
        } else {
            // If the new Bitcoin wallet (> 1.0.0 bdk) does not yet exist:
            // We check if we have an old (< 1.0.0 bdk) wallet. If so, we migrate.
            let export =
                Self::get_pre_1_0_bdk_wallet_export(data_dir, network, account, seed).await?;

            Self::create_new(
                xprivkey,
                network,
                account,
                dojo,
                require_dojo,
                client,
                connection,
                finality_confirmations,
                target_block,
                export,
                tauri_handle,
                false, // Outpost never calls clearnet fee APIs
            )
            .await
        }
    }

    /// Create a new wallet, persisted to an in-memory sqlite database.
    /// Should only be used for testing.
    pub async fn with_sqlite_in_memory(
        seed: &impl BitcoinWalletSeed,
        network: Network,
        account: WalletAccount,
        dojo: Option<Arc<DojoFeeEstimator>>,
        require_dojo: bool,
        electrum_rpc_urls: &[String],
        socks5_proxy: Option<String>,
        finality_confirmations: u32,
        target_block: u32,
        sync_interval: Duration,
        tauri_handle: TauriHandle,
    ) -> Result<Wallet<bdk_wallet::rusqlite::Connection, Client>> {
        Self::create_new(
            seed.derive_extended_private_key(network)?,
            network,
            account,
            dojo,
            require_dojo,
            Client::new(electrum_rpc_urls, sync_interval, socks5_proxy, network)
                .await
                .expect("Failed to create electrum client"),
            || {
                bdk_wallet::rusqlite::Connection::open_in_memory()
                    .context("Failed to open in-memory SQLite database")
            },
            finality_confirmations,
            target_block,
            None,
            tauri_handle,
            false, // Outpost never calls clearnet fee APIs
        )
        .await
    }

    /// Create a new wallet in the database and perform a full scan.
    /// This is a private API so we allow too many arguments.
    #[allow(clippy::too_many_arguments)]
    async fn create_new<Persister>(
        xprivkey: Xpriv,
        network: Network,
        account: WalletAccount,
        dojo: Option<Arc<DojoFeeEstimator>>,
        require_dojo: bool,
        client: Client,
        persister_constructor: impl FnOnce() -> Result<Persister>,
        finality_confirmations: u32,
        target_block: u32,
        old_wallet: Option<pre_1_0_0_bdk::Export>,
        tauri_handle: TauriHandle,
        use_mempool_space_fee_estimation: bool,
    ) -> Result<Wallet<Persister, Client>>
    where
        Persister: WalletPersister + Sized,
        <Persister as WalletPersister>::Error: std::error::Error + Send + Sync + 'static,
    {
        let external_descriptor = SamouraiBip84(xprivkey, KeychainKind::External, account)
            .build(network)
            .context("Failed to build external wallet descriptor")?;

        let internal_descriptor = SamouraiBip84(xprivkey, KeychainKind::Internal, account)
            .build(network)
            .context("Failed to build change wallet descriptor")?;

        // Build the wallet without a persister
        // because we create the persistence AFTER the full scan
        let mut wallet =
            bdk_wallet::Wallet::create(external_descriptor.clone(), internal_descriptor.clone())
                .network(network)
                .create_wallet_no_persist()
                .context("Failed to create persisterless wallet")?;

        // If we have an old wallet, we need to reveal the addresses that were used before
        // to speed up the initial sync.
        if let Some(old_wallet) = old_wallet {
            tracing::info!("Migrating from old Bitcoin wallet (< 1.0 bdk)");

            // We reveal the address but we DO NOT persist them yet
            // Because if we persist it'll create the wallet file and we will
            // not start the initial scan again if it's interrupted by the user
            let _ = wallet
                .reveal_addresses_to(KeychainKind::External, old_wallet.external_derivation_index);
            let _ = wallet
                .reveal_addresses_to(KeychainKind::Internal, old_wallet.internal_derivation_index);
        }

        tracing::info!("Starting initial Bitcoin wallet scan. This might take a while...");

        let progress_handle = tauri_handle.as_ref().map(|th| th.start_full_scan());

        let wallet = Arc::new(wallet);
        let ph = progress_handle.clone();
        let full_scan_response = client.inner.call("full_scan_wallet", move |electrum_client| {
            let callback = ph.clone().and_then(|ph| InnerSyncCallback::new(move |consumed, total| {
                ph.update(consumed, total);
            })).chain(InnerSyncCallback::new(move |consumed, total| {
                tracing::debug!(
                    "Full scanning Bitcoin wallet, currently at index {}. We will scan around {} in total.",
                    consumed,
                    total
                );
            }).throttle_callback(10.0)).to_full_scan_callback(Self::SCAN_STOP_GAP, 100);

            let full_scan = wallet.start_full_scan().inspect(callback);
            electrum_client.full_scan(full_scan, Self::SCAN_STOP_GAP as usize, Self::SCAN_BATCH_SIZE as usize, true)
        }).await?;

        // Only create the persister once we have the full scan result
        let mut persister = persister_constructor()?;

        // Create a new (persisted) wallet
        let mut wallet = bdk_wallet::Wallet::create(external_descriptor, internal_descriptor)
            .network(network)
            .create_wallet(&mut persister)
            .context("Failed to create wallet with persister")?;

        // Apply the full scan result to the wallet
        wallet.apply_update(full_scan_response)?;
        wallet.persist(&mut persister)?;

        progress_handle.map(|ph| ph.finish());

        tracing::trace!("Initial Bitcoin wallet scan completed");

        // Create the mempool client
        let mempool_client = if use_mempool_space_fee_estimation {
            mempool_client::MempoolClient::new(network).inspect_err(|e| {
                tracing::warn!("Failed to create mempool client: {:?}. We will only use the Electrum server for fee estimation.", e);
            }).ok()
        } else {
            None
        };

        // Create cached fee estimators
        let cached_electrum_fee_estimator = Arc::new(CachedFeeEstimator::new(client.clone()));
        let cached_mempool_fee_estimator =
            Arc::new(mempool_client.clone().map(CachedFeeEstimator::new));

        Ok(Wallet {
            wallet: wallet.into_arc_mutex_async(),
            electrum_client: Arc::new(client),
            cached_electrum_fee_estimator,
            cached_mempool_fee_estimator,
            persister: persister.into_arc_mutex_async(),
            tauri_handle,
            network,
            finality_confirmations,
            target_block,
            xprivkey,
            dojo: Arc::new(TokioRwLock::new(dojo)),
            require_dojo: Arc::new(AtomicBool::new(require_dojo)),
            sync_window: Arc::new(SyncWindow::default()),
        })
    }

    /// Load existing wallet data from the database
    #[allow(clippy::too_many_arguments)]
    async fn create_existing<Persister>(
        xprivkey: Xpriv,
        network: Network,
        account: WalletAccount,
        dojo: Option<Arc<DojoFeeEstimator>>,
        require_dojo: bool,
        client: Client,
        mut persister: Persister,
        finality_confirmations: u32,
        target_block: u32,
        tauri_handle: TauriHandle,
        use_mempool_space_fee_estimation: bool,
    ) -> Result<Wallet<Persister, Client>>
    where
        Persister: WalletPersister + Sized,
        <Persister as WalletPersister>::Error: std::error::Error + Send + Sync + 'static,
    {
        let external_descriptor = SamouraiBip84(xprivkey, KeychainKind::External, account)
            .build(network)
            .context("Failed to build external wallet descriptor")?;

        let internal_descriptor = SamouraiBip84(xprivkey, KeychainKind::Internal, account)
            .build(network)
            .context("Failed to build change wallet descriptor")?;

        tracing::debug!("Loading existing Bitcoin wallet from database");

        let wallet = bdk_wallet::Wallet::load()
            .descriptor(KeychainKind::External, Some(external_descriptor))
            .descriptor(KeychainKind::Internal, Some(internal_descriptor))
            .extract_keys()
            .load_wallet(&mut persister)
            .context("Failed to open database")?
            .context("No wallet found in database")?;

        // Create the mempool client with caching
        let cached_mempool_fee_estimator = if use_mempool_space_fee_estimation {
            mempool_client::MempoolClient::new(network).inspect_err(|e| {
                tracing::warn!("Failed to create mempool client: {:?}. We will only use the Electrum server for fee estimation.", e);
            }).ok().map(CachedFeeEstimator::new)
        } else {
            None
        };

        // Wrap the electrum client with caching
        let cached_electrum_fee_estimator = Arc::new(CachedFeeEstimator::new(client.clone()));

        let wallet = Wallet {
            wallet: wallet.into_arc_mutex_async(),
            electrum_client: Arc::new(client),
            cached_electrum_fee_estimator,
            cached_mempool_fee_estimator: Arc::new(cached_mempool_fee_estimator),
            persister: persister.into_arc_mutex_async(),
            tauri_handle,
            network,
            finality_confirmations,
            target_block,
            xprivkey,
            dojo: Arc::new(TokioRwLock::new(dojo)),
            require_dojo: Arc::new(AtomicBool::new(require_dojo)),
            sync_window: Arc::new(SyncWindow::default()),
        };

        Ok(wallet)
    }

    /// Broadcast the given transaction to the network and emit a tracing statement
    /// if done so successfully.
    ///
    /// Returns the transaction ID and a future for when the transaction meets
    /// the configured finality confirmations.
    pub async fn broadcast(
        &self,
        transaction: Transaction,
        kind: &str,
    ) -> Result<(Txid, Subscription)> {
        let txid = transaction.compute_txid();

        // to watch for confirmations, watching a single output is enough
        let subscription = self
            .subscribe_to(Box::new((
                txid,
                transaction.output[0].script_pubkey.clone(),
            )))
            .await;

        if let Some(dojo) = self.dojo.read().await.clone() {
            let tx_hex = bitcoin::consensus::encode::serialize_hex(&transaction);
            let published = broadcast_via_dojo(
                txid,
                kind,
                DOJO_BROADCAST_ATTEMPTS,
                DOJO_BROADCAST_RETRY_DELAY,
                || async {
                    match tokio::time::timeout(DOJO_BROADCAST_TIMEOUT, dojo.broadcast(&tx_hex))
                        .await
                    {
                        Ok(result) => result,
                        Err(_) => Err(anyhow!(
                            "Dojo broadcast timed out after {} seconds",
                            DOJO_BROADCAST_TIMEOUT.as_secs()
                        )),
                    }
                },
            )
            .await;

            if published {
                self.mark_unconfirmed(transaction).await?;
                return Ok((txid, subscription));
            }

            tracing::warn!(
                %txid,
                %kind,
                attempts = DOJO_BROADCAST_ATTEMPTS,
                "Dojo broadcast exhausted its attempts; falling back to Electrum"
            );
        }

        let broadcast_results = self
            .electrum_client
            .transaction_broadcast_all(&transaction)
            .await
            .with_context(|| {
                format!(
                    "Failed to broadcast Bitcoin {} transaction to any server {}",
                    kind, txid
                )
            })?;

        // Check if at least one broadcast succeeded
        let successful_count = broadcast_results.iter().filter(|r| r.is_ok()).count();
        let total_count = broadcast_results.len();

        if successful_count == 0 {
            // Collect all errors to create a MultiError
            let errors: Vec<_> = broadcast_results
                .into_iter()
                .filter_map(|result| result.err())
                .collect();

            let context = format!(
                "Bitcoin {} transaction {} failed to broadcast on all {} servers",
                kind, txid, total_count
            );

            let multi_error = electrum_pool::MultiError::new(errors, context);
            return Err(anyhow::Error::from(multi_error));
        }

        tracing::info!(
            %txid, %kind,
            successful_broadcasts = successful_count,
            total_servers = total_count,
            "Published Bitcoin transaction (accepted at {}/{} servers)",
            successful_count, total_count
        );

        // The transaction was accepted by the mempool
        // We know this because otherwise Electrum would have rejected it
        //

        self.mark_unconfirmed(transaction).await?;

        Ok((txid, subscription))
    }

    /// Mark a broadcast transaction as unconfirmed so it is included in the
    /// wallet balance immediately.
    async fn mark_unconfirmed(&self, transaction: Transaction) -> Result<()> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time went backwards")
            .as_secs();

        let mut wallet = self.wallet.lock().await;
        let mut persister = self.persister.lock().await;
        wallet.apply_unconfirmed_txs(vec![(transaction, timestamp)]);
        wallet.persist(&mut persister)?;
        Ok(())
    }

    /// Broadcast a persisted transaction only if it is not already known.
    /// On broadcast error, check once more to cover the race where the
    /// transaction reached the network but the server's history lagged.
    pub async fn broadcast_if_unseen(
        &self,
        transaction: Transaction,
        kind: &str,
    ) -> Result<(Txid, Subscription)> {
        let txid = transaction.compute_txid();
        let script = transaction
            .output
            .first()
            .context("Cannot broadcast a transaction without outputs")?
            .script_pubkey
            .clone();

        let broadcast_tx = transaction.clone();
        broadcast_if_unknown(
            txid,
            script.clone(),
            |key| async move { self.status_of_script(&key).await },
            || async { self.broadcast(broadcast_tx, kind).await.map(|_| ()) },
        )
        .await?;

        let subscription = self.subscribe_to(Box::new((txid, script))).await;
        Ok((txid, subscription))
    }

    /// Broadcast a transaction, but only if it's not already in the mempool/blockchain.
    /// Return txid and a subscription to it's status in either case.
    pub async fn ensure_broadcasted(
        &self,
        tx: Transaction,
        kind: &str,
    ) -> Result<(Txid, Subscription)> {
        let txid = tx.compute_txid();

        let status = self.status_of_script(&tx).await?;

        if matches!(status, ScriptStatus::InMempool | ScriptStatus::Confirmed(_)) {
            let subscription = self.subscribe_to(Box::new(tx)).await;
            return Ok((txid, subscription));
        }

        self.broadcast(tx, kind).await
    }

    pub async fn get_raw_transaction(&self, txid: Txid) -> Result<Option<Arc<Transaction>>> {
        self.get_tx(txid)
            .await
            .with_context(|| format!("Could not get raw tx with id: {}", txid))
    }

    /// Verify that every input of the PSBT references an existing unspent
    /// output (confirmed or in mempool) whose script and value match the
    /// claimed `witness_utxo`.
    pub async fn verify_psbt_prevouts(
        &self,
        psbt: &Psbt,
        cancel: &watch::Receiver<bool>,
    ) -> std::result::Result<(), PsbtPrevoutVerificationError> {
        if psbt.inputs.len() != psbt.unsigned_tx.input.len() {
            return Err(PsbtPrevoutVerificationError::InputMapCountMismatch {
                transaction_inputs: psbt.unsigned_tx.input.len(),
                input_maps: psbt.inputs.len(),
            });
        }

        let mut claims: HashMap<ScriptBuf, Vec<(bitcoin::OutPoint, Amount)>> = HashMap::new();
        for (index, (input, tx_input)) in
            psbt.inputs.iter().zip(&psbt.unsigned_tx.input).enumerate()
        {
            let Some(witness_utxo) = input.witness_utxo.as_ref() else {
                return Err(PsbtPrevoutVerificationError::MissingWitnessUtxo {
                    input_index: index,
                });
            };

            claims
                .entry(witness_utxo.script_pubkey.clone())
                .or_default()
                .push((tx_input.previous_output, witness_utxo.value));
        }

        if is_cancelled(cancel) {
            return Err(PsbtPrevoutVerificationError::Cancelled);
        }
        let (scripts, claims): (Vec<_>, Vec<_>) = claims.into_iter().unzip();
        let unspent = self.list_unspent(scripts, cancel).await?;

        for (outpoints, utxos) in claims.into_iter().zip(unspent) {
            for (outpoint, claimed_value) in outpoints {
                let Some(utxo) = utxos.iter().find(|utxo| {
                    utxo.tx_hash == outpoint.txid && utxo.tx_pos == outpoint.vout as usize
                }) else {
                    return Err(PsbtPrevoutVerificationError::PrevoutNotFound { outpoint });
                };

                let actual_value = Amount::from_sat(utxo.value);
                if actual_value != claimed_value {
                    return Err(PsbtPrevoutVerificationError::ValueMismatch {
                        outpoint,
                        actual: actual_value,
                        claimed: claimed_value,
                    });
                }
            }
        }

        Ok(())
    }

    /// Query each script separately on every server, then drain all started calls.
    /// Returned groups have the same order as `scripts`.
    ///
    /// A UTXO reported by any single server is accepted as existing; the
    /// responses of all servers are merged.
    /// Scripts are not batched: a server refusing one script (e.g. one with too
    /// many UTXOs) would fail the whole batch, and we could not tell it apart
    /// from a server problem.
    ///
    /// Once `cancel` fires, each server stops before its next script.
    async fn list_unspent(
        &self,
        scripts: Vec<ScriptBuf>,
        cancel: &watch::Receiver<bool>,
    ) -> Result<Vec<Vec<bdk_electrum::electrum_client::ListUnspentRes>>, PsbtPrevoutVerificationError>
    {
        let query_scripts = scripts.clone();
        let query_cancel = cancel.clone();
        let results = self
            .electrum_client
            .inner
            .join_quorum_and_drain("script_list_unspent", move |client| {
                query_scripts
                    .iter()
                    .map(|script| {
                        if is_cancelled(&query_cancel) {
                            return Err(bdk_electrum::electrum_client::Error::Message(
                                "Lock input check was cancelled".to_owned(),
                            ));
                        }
                        match client.inner.script_list_unspent(script) {
                            Ok(utxos) => Ok(Ok(utxos)),
                            // The server answered with an error for this script only
                            Err(bdk_electrum::electrum_client::Error::Protocol(refusal)) => {
                                Ok(Err(refusal))
                            }
                            Err(error) => Err(error),
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .await
            .context("Failed to query Electrum servers for unspent outputs")
            .map_err(PsbtPrevoutVerificationError::Electrum)?;
        if is_cancelled(cancel) {
            return Err(PsbtPrevoutVerificationError::Cancelled);
        }

        let mut answers = Vec::new();
        let mut errors = Vec::new();
        for result in results {
            match result {
                Ok(groups) => answers.push(groups),
                Err(error) => {
                    tracing::debug!(%error, "Electrum server failed to list unspent outputs");
                    errors.push(error);
                }
            }
        }
        tracing::debug!(
            scripts = scripts.len(),
            answered = answers.len(),
            failed = errors.len(),
            "Listed unspent outputs of PSBT input scripts"
        );

        if answers.is_empty() {
            return Err(PsbtPrevoutVerificationError::Electrum(
                electrum_pool::MultiError::new(
                    errors,
                    "No Electrum server returned unspent outputs for scripts".to_owned(),
                )
                .into(),
            ));
        }

        let mut per_script = vec![Vec::new(); scripts.len()];
        for groups in answers {
            for (script_answers, answer) in per_script.iter_mut().zip(groups) {
                script_answers.push(answer);
            }
        }

        scripts
            .into_iter()
            .zip(per_script)
            .map(|(script, answers)| {
                let (listed, refused): (Vec<_>, Vec<_>) =
                    answers.into_iter().partition(Result::is_ok);
                if !refused.is_empty() {
                    tracing::debug!(%script, refused = refused.len(), listed = listed.len(), "Electrum servers refused to list a script");
                }
                if listed.is_empty()
                    && let Some(Err(refusal)) = refused.into_iter().next()
                {
                    return Err(PsbtPrevoutVerificationError::ScriptRefused {
                        script,
                        refusal: refusal.to_string(),
                    });
                }
                Ok(listed.into_iter().flatten().flatten().collect())
            })
            .collect()
    }

    // Returns the TxId of the last published Bitcoin transaction
    pub async fn last_published_txid(&self) -> Result<Txid> {
        let wallet = self.wallet.lock().await;

        // Get all the transactions sorted by recency
        let mut txs = wallet.transactions().collect::<Vec<_>>();
        txs.sort_by(|tx1, tx2| tx2.chain_position.cmp(&tx1.chain_position));

        let last_tx = txs.first().context("No transactions found")?;

        Ok(last_tx.tx_node.txid)
    }

    pub async fn status_of_script(&self, tx: &dyn Watchable) -> Result<ScriptStatus> {
        self.electrum_client.status_of_script(tx, true).await
    }

    pub async fn subscribe_to(&self, tx: Box<dyn Watchable>) -> Subscription {
        let txid = tx.id();
        let script = tx.script();
        let dojo = self.dojo.read().await.clone();
        let idle_timeout = self.electrum_client.subscription_idle_timeout;

        let initial_status = match self.electrum_client.status_of_script(&tx, false).await {
            Ok(status) => Some(status),
            Err(err) => {
                tracing::debug!(%txid, %err, "Failed to get initial status for subscription. We won't notify the caller and will try again later.");
                None
            }
        };

        let mut subscriptions = self.electrum_client.subscriptions.lock().await;

        let sender = subscriptions
            .entry((txid, script.clone()))
            .or_insert_with(|| {
                let (sender, _) = watch::channel(ScriptStatus::Unseen);
                if let Some(status) = initial_status {
                    sender.send_replace(status);
                }
                let client = self.electrum_client.clone();
                let task_sender = sender.clone();

                tokio::spawn(async move {
                    let mut last_status = initial_status;
                    let mut idle_since: Option<Instant> = None;

                    loop {
                        let new_status = client
                            .status_of_script(&tx, false)
                            .await
                            .unwrap_or_else(|error| {
                                tracing::warn!(%txid, error = ?error, "Failed to get status of script");
                                ScriptStatus::Retrying
                            });

                        if new_status != ScriptStatus::Retrying {
                            last_status = Some(trace_status_change(txid, last_status, new_status));
                            let _ = task_sender.send(new_status);
                        }

                        if task_sender.receiver_count() > 0 {
                            idle_since = None;
                        } else if idle_since.get_or_insert_with(Instant::now).elapsed()
                            >= idle_timeout
                        {
                            let mut subscriptions = client.subscriptions.lock().await;

                            if subscriptions
                                .get(&(txid, script.clone()))
                                .is_some_and(|sender| sender.receiver_count() == 0)
                            {
                                tracing::debug!(%txid, ?idle_timeout, "No subscribers for transaction status, dropping subscription");
                                subscriptions.remove(&(txid, script));
                                return;
                            }

                            idle_since = None;
                        }

                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }.instrument(debug_span!("BitcoinWalletSubscription")));

                sender
            });

        let checker_client = self.electrum_client.clone();
        Subscription {
            receiver: sender.subscribe(),
            finality_confirmations: self.finality_confirmations,
            txid,
            dojo,
            inclusion: Some(crate::primitives::InclusionChecker(Arc::new(
                move |txid, height| {
                    let client = checker_client.clone();
                    Box::pin(async move {
                        Ok(client.verify_inclusion_at(txid, height).await?
                            == InclusionVerdict::Proven)
                    })
                },
            ))),
        }
    }

    pub async fn active_subscription_count(&self) -> usize {
        self.electrum_client.subscriptions.lock().await.len()
    }

    pub async fn wallet_export(&self, role: &str) -> Result<FullyNodedExport> {
        let wallet = self.wallet.lock().await;
        match bdk_wallet::export::FullyNodedExport::export_wallet(
            &wallet,
            &format!("{}-{}", role, self.network),
            true,
        ) {
            Result::Ok(wallet_export) => Ok(wallet_export),
            Err(err_msg) => Err(anyhow::Error::msg(err_msg)),
        }
    }

    /// Get a transaction from the Electrum server or the cache.
    pub async fn get_tx(&self, txid: Txid) -> Result<Option<Arc<Transaction>>> {
        let tx = self
            .electrum_client
            .get_tx(txid)
            .await
            .context("Failed to get transaction from cache or Electrum server")?;

        Ok(tx)
    }

    /// Seeds the wallet's transaction graph from the paired Dojo's view of the
    /// account, so an unconfirmed deposit is visible before (or without) an
    /// Electrum script-history sync.
    ///
    /// Discovery comes from Dojo — the same indexer the paired GUI wallet uses.
    /// Electrum only supplies the raw transaction bytes by txid, which works
    /// even when its script index is stale. Confirmed state still comes from the
    /// regular sync; this method only adds unconfirmed transactions.
    pub async fn seed_from_dojo(&self) -> Result<usize> {
        let Some(dojo) = self.dojo.read().await.clone() else {
            return Ok(0);
        };

        let xpubs: Vec<String> = {
            let wallet = self.wallet.lock().await;
            [KeychainKind::External, KeychainKind::Internal]
                .into_iter()
                .filter_map(|keychain| {
                    account_xpub(wallet.public_descriptor(keychain).to_string().as_str())
                })
                .collect()
        };
        if xpubs.is_empty() {
            return Ok(0);
        }

        let utxos = dojo.unspent(&xpubs).await?;
        let known: std::collections::HashSet<Txid> = {
            let wallet = self.wallet.lock().await;
            wallet.transactions().map(|tx| tx.tx_node.txid).collect()
        };

        let mut additions = Vec::new();
        for utxo in utxos.iter().filter(|utxo| utxo.confirmations == 0) {
            let Ok(txid) = utxo.txid.parse::<Txid>() else {
                continue;
            };
            if known.contains(&txid) {
                continue;
            }
            if let Some(tx) = self.get_tx(txid).await? {
                additions.push(((*tx).clone(), now_secs()));
            }
        }
        if additions.is_empty() {
            return Ok(0);
        }

        let added = additions.len();
        let mut wallet = self.wallet.lock().await;
        wallet.apply_unconfirmed_txs(additions);
        let mut persister = self.persister.lock().await;
        wallet.persist(&mut persister)?;

        Ok(added)
    }

    /// Create a vector of sync requests
    ///
    /// This splits up all the revealed spks and builds a sync request for each chunk.
    /// Useful for syncing the whole wallet in chunks.
    async fn chunked_sync_request(
        &self,
        max_num_chunks: u32,
        batch_size: u32,
    ) -> Vec<SyncRequestBuilderFactory> {
        #[allow(clippy::type_complexity)]
        let (spks, chain_tip): (Vec<((KeychainKind, u32), ScriptBuf)>, CheckPoint) = {
            let wallet = self.wallet.lock().await;

            let spks = self.window_spks(&wallet);

            let chain_tip = wallet.local_chain().tip();

            (spks, chain_tip)
        };

        let total_spks =
            u32::try_from(spks.len()).expect("Number of SPKs should not exceed u32::MAX");

        if total_spks == 0 {
            tracing::debug!("Not syncing because there are no spks in our wallet");
            return vec![];
        }

        // We only use as many chunks as are useful to reduce the number of requests
        // given the batch size
        // This means: num_chunks * batch_size < total number of spks
        //
        // E.g we have 1000 spks and a batch size of 100, we only use 10 chunks at most
        // If we used 20 chunks we would not maximize the batch size because
        // each chunk would have 50 spks (which is less than the batch size)
        //
        // At least one chunk is always required. At most total_spks / batch_size or the provided num_chunks (whichever is smaller)
        let num_chunks = max_num_chunks.min(total_spks / batch_size).max(1);
        let chunk_size = total_spks.div_ceil(num_chunks);

        let mut chunks = Vec::new();

        for spk_chunk in spks.chunks(chunk_size as usize) {
            let factory = SyncRequestBuilderFactory {
                chain_tip: chain_tip.clone(),
                spks: spk_chunk.to_vec(),
            };
            chunks.push(factory);
        }

        chunks
    }

    /// Sync the wallet with the Blockchain
    /// Spawn `num_chunks` tasks to sync the wallet in parallel
    /// Call the callback with the cumulative progress of the sync
    pub async fn chunked_sync_with_callback(&self, callback: sync_ext::SyncCallback) -> Result<()> {
        // Construct the chunks to process
        let sync_request_factories = self
            .chunked_sync_request(Self::SCAN_CHUNKS, Self::SCAN_BATCH_SIZE)
            .await;

        tracing::debug!(
            "Starting to sync Bitcoin wallet with {} concurrent chunks and batch size of {}",
            sync_request_factories.len(),
            Self::SCAN_BATCH_SIZE
        );
        let sync_chunk_count = sync_request_factories.len();
        let sync_spk_count: usize = sync_request_factories.iter().map(|f| f.spks.len()).sum();

        // For each sync request, store the latest progress update in a HashMap keyed by the index of the chunk
        let cumulative_progress_handle = sync_ext::CumulativeProgress::new().into_arc_mutex_sync(); // Use the newtype here

        // Assign each sync request:
        // 1. its individual callback which links back to the CumulativeProgress
        // 2. its chunk of the SyncRequest
        let sync_requests = sync_request_factories
            .into_iter()
            .enumerate()
            .map(|(index, sync_request_factory)| {
                let callback = cumulative_progress_handle
                    .clone()
                    .chunk_callback(callback.clone(), index as u64);

                (callback, sync_request_factory)
            })
            .collect::<Vec<_>>();

        // Create a vector of futures to process in parallel
        let futures = sync_requests
            .into_iter()
            .map(|(callback, sync_request_factory)| {
                self.sync_with_custom_callback(sync_request_factory, callback)
                    .in_current_span()
            });

        // Start timer to measure the time taken to sync the wallet
        let start_time = Instant::now();

        // Execute all futures concurrently and collect results
        let results = futures::future::join_all(futures).await;

        // Check if any requests failed
        for result in results {
            result?;
        }

        // Calculate the time taken to sync the wallet
        let duration = start_time.elapsed();
        tracing::trace!(
            "Synced Bitcoin wallet in {:?} with {} concurrent chunks and batch size {}",
            duration,
            Self::SCAN_CHUNKS,
            Self::SCAN_BATCH_SIZE
        );
        // Info-level so a slow sync can be explained from the swap log: how many
        // addresses were checked and how many chunks they were split into.
        tracing::info!(
            elapsed_ms = duration.as_millis() as u64,
            addresses = sync_spk_count,
            chunks = sync_chunk_count,
            batch_size = Self::SCAN_BATCH_SIZE,
            "Electrum wallet sync finished"
        );

        Ok(())
    }

    /// Sync the wallet with the blockchain, optionally calling a callback on progress updates.
    /// This will NOT emit progress events to the UI.
    ///
    /// If no sync request is provided, we default to syncing all revealed spks.
    pub async fn sync_with_custom_callback(
        &self,
        sync_request_factory: SyncRequestBuilderFactory,
        callback: InnerSyncCallback,
    ) -> Result<()> {
        let callback = Arc::new(SyncMutex::new(callback));

        // The Electrum client's transaction cache starts empty, and BDK then downloads every
        // transaction of the wallet's history, and the previous transaction of each of their
        // inputs, one request at a time. This wallet has ~60 of them: ~86 round trips, a
        // 60-70 s sync that grows with every swap. Hand the client what the wallet already
        // stores so only new transactions are fetched.
        let known_txs = Arc::new({
            let wallet = self.wallet.lock().await;
            known_transactions(&wallet)
        });

        let sync_response = self
            .electrum_client
            .inner
            .call("sync_wallet", move |client| {
                let sync_request_factory = sync_request_factory.clone();
                let callback = callback.clone();
                client.populate_tx_cache(known_txs.iter().cloned());

                // Time the phases of the sync: when the first and the last of the requested
                // items (scripts) were answered, and how long the rest took (fetching
                // transactions, previous outputs and anchors). A 60 s sync was opaque.
                let started = Instant::now();
                let item_times = Arc::new(std::sync::Mutex::new((None::<Duration>, None::<Duration>, 0u64)));
                let timing = item_times.clone();

                // Build the sync request
                let sync_request = sync_request_factory
                    .build()
                    .inspect(move |_, progress| {
                        if let Ok(mut guard) = callback.lock() {
                            guard.call(progress.consumed() as u64, progress.total() as u64);
                        }
                        if let Ok(mut times) = timing.lock() {
                            let elapsed = started.elapsed();
                            times.0.get_or_insert(elapsed);
                            times.1 = Some(elapsed);
                            times.2 = progress.consumed() as u64;
                        }
                    })
                    .build();

                // `false`: do not fetch the previous output of every input (another request
                // per external transaction, on every sync). The wallet keeps the ones it has;
                // only fee calculation for a transaction paid from outside needs them, and no
                // shipped path asks for that.
                let result = client.sync(sync_request, Self::SCAN_BATCH_SIZE as usize, false);
                if let Ok(times) = item_times.lock() {
                    let total = started.elapsed();
                    tracing::info!(
                        total_ms = total.as_millis() as u64,
                        first_item_ms = times.0.map(|d| d.as_millis() as u64),
                        last_item_ms = times.1.map(|d| d.as_millis() as u64),
                        items = times.2,
                        after_last_item_ms = times.1.map(|d| total.saturating_sub(d).as_millis() as u64),
                        ok = result.is_ok(),
                        "Electrum sync call finished"
                    );
                }
                result
            })
            .await?;

        // We only acquire the lock after the long running .sync(...) call has finished
        let mut wallet = self.wallet.lock().await;
        wallet.apply_update(sync_response)?; // Use the full sync_response, not just chain_update

        let mut persister = self.persister.lock().await;
        wallet.persist(&mut persister)?;

        Ok(())
    }

    /// Perform a single sync of the wallet with the blockchain
    /// and emit progress events to the UI.
    async fn sync_once(&self) -> Result<()> {
        let background_process_handle = self.tauri_handle.as_ref().map(|th| th.start_sync());

        // We want to update the UI as often as possible
        let tauri_callback = background_process_handle.clone().and_then(|bph| {
            sync_ext::InnerSyncCallback::new(move |consumed, total| {
                bph.update(consumed, total);
            })
        });

        // We throttle the tracing logging to 10% increments
        let tracing_callback = sync_ext::InnerSyncCallback::new(move |consumed, total| {
            tracing::debug!("Syncing Bitcoin wallet ({}/{})", consumed, total);
        })
        .throttle_callback(10.0);

        // We chain the callbacks and then initiate the sync
        self.chunked_sync_with_callback(tauri_callback.chain(tracing_callback).finalize())
            .await?;

        background_process_handle.map(|bph| bph.finish());

        Ok(())
    }

    /// Sync the wallet with the blockchain and emit progress events to the UI.
    /// Retries the sync if it fails using an exponential backoff.
    pub async fn sync(&self) -> Result<()> {
        let backoff = backoff::ExponentialBackoffBuilder::new()
            .with_max_elapsed_time(Some(Self::SYNC_MAX_ELAPSED_TIME))
            .with_max_interval(Duration::from_secs(1))
            .build();

        backoff::future::retry_notify(
            backoff,
            || async { self.sync_once().await.map_err(backoff::Error::transient) },
            |err, wait_time: Duration| {
                tracing::warn!(
                    ?err,
                    "Failed to sync Bitcoin wallet. We will retry in {} seconds",
                    wait_time.as_secs()
                );
            },
        )
        .await
        .context("Failed to sync Bitcoin wallet after retries")
    }

    pub async fn health_check(&self) -> Result<()> {
        self.electrum_client
            .update_block_height()
            .await
            .context("Bitcoin wallet failed to reach the Electrum backend")
    }

    /// Calculate the fee for a given transaction.
    ///
    /// Will fail if the transaction inputs are not owned by this wallet.
    pub async fn transaction_fee(&self, txid: Txid) -> Result<Amount> {
        // Ensure wallet is synced before getting transaction
        self.sync().await?;

        let transaction = self
            .get_tx(txid)
            .await
            .context(
                "Could not fetch transaction from Electrum server while trying to determine fees",
            )?
            .ok_or_else(|| anyhow!("Transaction not found"))?;

        let fee = self.wallet.lock().await.calculate_fee(&transaction)?;

        Ok(fee)
    }
}

// These are the methods that are always available, regardless of the persister.
impl<T, C> Wallet<T, C> {
    /// Get the network of this wallet.
    pub fn network(&self) -> Network {
        self.network
    }

    /// Get the finality confirmations of this wallet.
    pub fn finality_confirmations(&self) -> u32 {
        self.finality_confirmations
    }

    /// Get the target block of this wallet.
    ///
    /// This is the the number of blocks we want to wait at most for
    /// one ofour transaction to be confirmed.
    pub fn target_block(&self) -> u32 {
        self.target_block
    }

    /// Replace the paired Dojo estimator after construction and start its
    /// warm-up. This is used once the GUI/ASB supplies decrypted credentials.
    pub async fn set_dojo_fee_estimator(&self, dojo: Arc<DojoFeeEstimator>) {
        dojo.spawn_warm_up();
        *self.dojo.write().await = Some(dojo);
    }

    /// Require Dojo for fresh fee estimates without configuring an estimator.
    /// Used for recovery-only resume, where already-signed fees may proceed but
    /// a new estimate must fail closed instead of falling back to Electrum.
    pub fn require_dojo_fee_source(&self) {
        self.require_dojo.store(true, Ordering::SeqCst);
    }

    /// Insert a wallet-owned UTXO without publishing it to the backing chain.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn insert_fake_utxo(&self, amount: Amount) -> bitcoin::OutPoint {
        let mut wallet = self.wallet.lock().await;
        bdk_wallet::test_utils::receive_output_in_latest_block(&mut wallet, amount)
    }

    /// Prove control of the keys behind every input of a PSBT built by this
    /// wallet, using the BIP-32 derivation entries bdk records per input.
    pub fn prove_utxo_ownership(
        &self,
        psbt: &Psbt,
        challenge: utxo_ownership_proof::Challenge,
    ) -> Result<Vec<utxo_ownership_proof::OwnershipProof>> {
        let secp = bitcoin::secp256k1::Secp256k1::new();
        let root_fingerprint = self.xprivkey.fingerprint(&secp);

        psbt.inputs
            .iter()
            .enumerate()
            .map(|(index, input)| {
                let (public_key, (_, path)) = input
                    .bip32_derivation
                    .iter()
                    .find(|(_, (fingerprint, _))| *fingerprint == root_fingerprint)
                    .with_context(|| {
                        format!("Input {index} has no derivation entry for our wallet root")
                    })?;

                let derived = self
                    .xprivkey
                    .derive_priv(&secp, path)
                    .with_context(|| format!("Failed to derive key for input {index}"))?;

                if derived.private_key.public_key(&secp) != *public_key {
                    bail!("Derived key for input {index} does not match its derivation entry");
                }

                Ok(utxo_ownership_proof::sign(&derived.private_key, challenge))
            })
            .collect()
    }
}

impl<Persister, C> Wallet<Persister, C>
where
    Persister: WalletPersister + Sized,
    <Persister as WalletPersister>::Error: std::error::Error + Send + Sync + 'static,
    C: EstimateFeeRate + Send + Sync + 'static,
{
    /// The revealed scripts a sync should check: all of them, or only those inside
    /// the [`SyncWindow`] once the account's real position is known.
    fn window_spks(
        &self,
        wallet: &PersistedWallet<Persister>,
    ) -> Vec<((KeychainKind, u32), ScriptBuf)> {
        let index = wallet.spk_index();
        let limits = self.sync_window.floors().map(|(receive_floor, change_floor)| {
            (
                sync_window_end(index.last_used_index(KeychainKind::External), receive_floor),
                sync_window_end(index.last_used_index(KeychainKind::Internal), change_floor),
            )
        });

        index
            .revealed_spks(..)
            .filter(|((keychain, position), _)| match limits {
                None => true,
                Some((receive_end, change_end)) => match keychain {
                    KeychainKind::External => *position < receive_end,
                    KeychainKind::Internal => *position < change_end,
                },
            })
            .map(|(position, spk)| (position, spk.clone()))
            .collect()
    }

    /// How many scripts the next sync will ask the server about.
    pub async fn sync_script_count(&self) -> usize {
        let wallet = self.wallet.lock().await;
        self.window_spks(&wallet).len()
    }

    /// Returns the combined fee rate from the Electrum and Mempool clients.
    ///
    /// If the mempool client is not available, we use the Electrum client.
    /// If the mempool client is available, we use the higher of the two.
    /// If either of the clients fail but the other is successful, we use the successful one.
    /// If both clients fail, we return an error
    /// Raw fee rate at the requested target block, before Outpost's policy caps.
    pub async fn fee_rate_at(&self, target_block: u32) -> Result<FeeRate> {
        self.combined_fee_rate_at(target_block).await
    }

    /// Raw minimum relay fee rate, before Outpost's policy caps.
    pub async fn min_relay_fee_rate(&self) -> Result<FeeRate> {
        self.combined_min_relay_fee().await
    }

    async fn combined_fee_rate(&self) -> Result<FeeRate> {
        self.combined_fee_rate_at(self.target_block).await
    }

    async fn combined_fee_rate_at(&self, target_block: u32) -> Result<FeeRate> {
        if let Some(dojo) = self.dojo.read().await.clone() {
            match dojo.estimate_feerate_async(target_block as usize).await {
                Ok(rate) => {
                    tracing::debug!(
                        dojo_rate_sat_vb = rate.to_sat_per_vb_ceil(),
                        "Using Dojo fee rate"
                    );
                    return Ok(rate);
                }
                Err(error) if self.require_dojo.load(Ordering::SeqCst) => {
                    return Err(error.context(
                        "Dojo fee estimation is required but failed; refusing to fall back to Electrum",
                    ));
                }
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        "Dojo fee estimation failed; falling back to Electrum"
                    );
                }
            }
        }

        let electrum_future = self
            .cached_electrum_fee_estimator
            .estimate_feerate(target_block);
        let mempool_future = async {
            match self.cached_mempool_fee_estimator.as_ref() {
                Some(mempool_client) => mempool_client
                    .estimate_feerate(target_block)
                    .await
                    .map(Some),
                None => Ok(None),
            }
        };

        let (electrum_result, mempool_result) = tokio::join!(electrum_future, mempool_future);

        match (electrum_result, mempool_result) {
            // If both sources are successful, we use the higher one
            (Ok(electrum_rate), Ok(Some(mempool_space_rate))) => {
                tracing::debug!(
                    electrum_rate_sat_vb = electrum_rate.to_sat_per_vb_ceil(),
                    mempool_space_rate_sat_vb = mempool_space_rate.to_sat_per_vb_ceil(),
                    "Successfully fetched fee rates from both Electrum and mempool.space. We will use the higher one"
                );
                Ok(std::cmp::max(electrum_rate, mempool_space_rate))
            }
            // If the Electrum source is successful
            // but we don't have a mempool client, we use the Electrum rate
            (Ok(electrum_rate), Ok(None)) => {
                tracing::trace!(
                    electrum_rate_sat_vb = electrum_rate.to_sat_per_vb_ceil(),
                    "No mempool.space client available, using Electrum rate"
                );
                Ok(electrum_rate)
            }
            // If the Electrum source is successful
            // but the mempool source fails, we use the Electrum rate
            (Ok(electrum_rate), Err(mempool_error)) => {
                tracing::warn!(
                    ?mempool_error,
                    electrum_rate_sat_vb = electrum_rate.to_sat_per_vb_ceil(),
                    "Failed to fetch mempool.space fee rate, using Electrum rate"
                );
                Ok(electrum_rate)
            }
            // If the mempool source is successful
            // but the Electrum source fails, we use the mempool rate
            (Err(electrum_error), Ok(Some(mempool_rate))) => {
                tracing::warn!(
                    ?electrum_error,
                    mempool_rate_sat_vb = mempool_rate.to_sat_per_vb_ceil(),
                    "Electrum fee rate failed, using mempool.space rate"
                );
                Ok(mempool_rate)
            }
            // If both sources fail, we return the error
            (Err(electrum_error), Err(mempool_error)) => {
                tracing::error!(
                    ?electrum_error,
                    ?mempool_error,
                    "Failed to fetch fee rates from both Electrum and mempool.space"
                );

                Err(electrum_error)
            }
            // If the Electrum source fails and the mempool source is not available, we return the Electrum error
            (Err(electrum_error), Ok(None)) => {
                tracing::warn!(
                    ?electrum_error,
                    "Electrum failed and mempool.space client is not available"
                );
                Err(electrum_error)
            }
        }
    }

    /// Returns the minimum relay fee from the Electrum and Mempool clients.
    ///
    /// Only fails if both sources fail. Always chooses the higher value.
    async fn combined_min_relay_fee(&self) -> Result<FeeRate> {
        let electrum_future = self.cached_electrum_fee_estimator.min_relay_fee();
        let mempool_future = async {
            match self.cached_mempool_fee_estimator.as_ref() {
                Some(mempool_client) => mempool_client.min_relay_fee().await.map(Some),
                None => Ok(None),
            }
        };

        let (electrum_result, mempool_result) = tokio::join!(electrum_future, mempool_future);

        match (electrum_result, mempool_result) {
            (Ok(electrum_fee), Ok(Some(mempool_space_fee))) => {
                tracing::trace!(
                    electrum_fee = ?electrum_fee,
                    mempool_space_fee = ?mempool_space_fee,
                    "Successfully fetched min relay fee from both Electrum and mempool.space. We will use the higher value"
                );
                Ok(std::cmp::max(electrum_fee, mempool_space_fee))
            }
            (Ok(electrum_fee), Ok(None)) => {
                tracing::trace!(
                    ?electrum_fee,
                    "No mempool.space client available, using Electrum min relay fee"
                );
                Ok(electrum_fee)
            }
            (Ok(electrum_fee), Err(mempool_space_error)) => {
                tracing::warn!(
                    ?mempool_space_error,
                    ?electrum_fee,
                    "Failed to fetch mempool.space min relay fee, using Electrum min relay fee"
                );
                Ok(electrum_fee)
            }
            (Err(electrum_error), Ok(Some(mempool_space_fee))) => {
                tracing::warn!(
                    ?electrum_error,
                    ?mempool_space_fee,
                    "Failed to fetch Electrum min relay fee, using mempool.space min relay fee"
                );
                Ok(mempool_space_fee)
            }
            (Err(electrum_error), Ok(None)) => Err(electrum_error.context(
                "Failed to fetch min relay fee from Electrum, and no mempool.space client available",
            )),
            (Err(electrum_error), Err(mempool_space_error)) => Err(electrum_error
                .context(mempool_space_error)
                .context("Failed to fetch min relay fee from both Electrum and mempool.space")),
        }
    }

    pub async fn sign_and_finalize(&self, mut psbt: Psbt) -> Result<Transaction> {
        // Acquire the wallet lock once here for efficiency within the non-finalized block
        let wallet_guard = self.wallet.lock().await;

        let finalized = wallet_guard.sign(&mut psbt, Default::default())?;

        if !finalized {
            bail!("PSBT is not finalized")
        }

        // Release the lock if finalization succeeded
        drop(wallet_guard);

        let tx = psbt.extract_tx();
        Ok(tx?)
    }

    /// Returns the total Bitcoin balance, which includes pending funds
    pub async fn balance(&self) -> Result<Amount> {
        Ok(self.wallet.lock().await.balance().total())
    }

    /// Returns the balance info of the wallet, including unconfirmed funds etc.
    pub async fn balance_info(&self) -> Result<Balance> {
        Ok(self.wallet.lock().await.balance())
    }

    /// Reveals the next address from the wallet.
    pub async fn new_address(&self) -> Result<Address> {
        let mut wallet = self.wallet.lock().await;

        // Only reveal a new address if absolutely necessary
        // We want to avoid revealing more and more addresses
        let address = wallet.next_unused_address(KeychainKind::External).address;

        // Important: persist that we revealed a new address.
        // Otherwise the wallet might reuse it (bad).
        let mut persister = self.persister.lock().await;
        wallet.persist(&mut persister)?;

        Ok(address)
    }

    pub async fn new_change_address(&self) -> Result<Address> {
        let mut wallet = self.wallet.lock().await;
        let address = wallet.next_unused_address(KeychainKind::Internal).address;
        let mut persister = self.persister.lock().await;
        wallet.persist(&mut persister)?;
        Ok(address)
    }

    /// Read-only next derivation index. Does not reveal or persist an address.
    pub async fn next_derivation_index(&self, keychain: KeychainKind) -> u32 {
        self.wallet.lock().await.next_derivation_index(keychain)
    }

    /// One past the highest derivation index that has actually received or spent
    /// funds (0 if none). Unlike `next_derivation_index` this ignores addresses
    /// revealed only as lookahead, so it cannot feed back into the address cache.
    pub async fn next_used_derivation_index(&self, keychain: KeychainKind) -> u32 {
        self.wallet
            .lock()
            .await
            .spk_index()
            .last_used_index(keychain)
            .map(|index| index.saturating_add(1))
            .unwrap_or(0)
    }

    pub async fn seed_address_cache(
        &self,
        receive_index: u32,
        change_index: u32,
        cache_size: u32,
    ) -> Result<()> {
        let mut wallet = self.wallet.lock().await;
        let mut persister = self.persister.lock().await;

        if receive_index > 0 {
            wallet
                .reveal_addresses_to(KeychainKind::External, receive_index)
                .count();
        }
        if change_index > 0 {
            wallet
                .reveal_addresses_to(KeychainKind::Internal, change_index)
                .count();
        }
        if cache_size > 0 {
            wallet
                .reveal_addresses_to(KeychainKind::External, cache_size)
                .count();
            wallet
                .reveal_addresses_to(KeychainKind::Internal, cache_size)
                .count();
        }
        wallet.persist(&mut persister)?;
        // From here on a sync only checks the part of the account that can hold funds.
        self.sync_window.set(receive_index, change_index);
        Ok(())
    }

    /// Builds a partially signed transaction that sends
    /// the given amount to the given address.
    /// The fee is calculated based on the weight of the transaction
    /// and the state of the current mempool.
    pub async fn send_to_address_dynamic_fee(
        &self,
        address: Address,
        amount: Amount,
        change_override: Option<Address>,
        max_inputs: Option<usize>,
    ) -> Result<PartiallySignedTransaction> {
        // Check address and change address for network equality.
        let address = bitcoin_address::revalidate_network(address, self.network)?;

        change_override
            .as_ref()
            .map(|a| bitcoin_address::revalidate_network(a.clone(), self.network))
            .transpose()
            .context("Change address is not on the correct network")?;

        let script = address.script_pubkey();

        let psbt = {
            let mut wallet = self.wallet.lock().await;

            // Build the transaction with a dummy fee rate
            // just to figure out the final weight of the transaction
            // send_to_address(...) takes an absolute fee
            let mut tx_builder = coin_selection::build_tx(&mut wallet, max_inputs, false)?;

            tx_builder.add_recipient(script.clone(), amount);
            tx_builder.fee_absolute(Amount::ZERO);

            tx_builder
                .finish()
                .context("Failed to select transaction inputs")?
        };

        let weight = estimated_signed_wallet_weight(&psbt)?;
        let fee = self.estimate_fee(weight, Some(amount)).await?;

        self.send_to_address(address, amount, fee, change_override, max_inputs)
            .await
    }

    /// Builds a partially signed transaction that sweeps our entire balance
    /// to a single address.
    ///
    /// The fee is calculated based on the weight of the transaction
    /// and the state of the current mempool.
    pub async fn sweep_balance_to_address_dynamic_fee(
        &self,
        address: Address,
    ) -> Result<PartiallySignedTransaction> {
        let (max_giveable, fee) = self
            .max_giveable(address.script_pubkey().len(), None)
            .await?;

        self.send_to_address(address, max_giveable, fee, None, None)
            .await
    }

    /// Builds a partially signed transaction that sends
    /// the given amount to the given address with the given
    /// absolute fee.
    ///
    /// Ensures that the address script is at output index `0`
    /// for the partially signed transaction.
    pub async fn send_to_address(
        &self,
        address: Address,
        amount: Amount,
        spending_fee: Amount,
        change_override: Option<Address>,
        max_inputs: Option<usize>,
    ) -> Result<PartiallySignedTransaction> {
        // Check address and change address for network equality.
        let address = bitcoin_address::revalidate_network(address, self.network)?;

        change_override
            .as_ref()
            .map(|a| bitcoin_address::revalidate_network(a.clone(), self.network))
            .transpose()
            .context("Change address is not on the correct network")?;

        let mut wallet = self.wallet.lock().await;
        let script = address.script_pubkey();

        // Build the transaction with a manual fee
        let mut tx_builder = coin_selection::build_tx(&mut wallet, max_inputs, false)?;
        tx_builder.add_recipient(script.clone(), amount);
        tx_builder.fee_absolute(spending_fee);

        let mut psbt = tx_builder
            .finish()
            .context("Failed to select transaction inputs")?;

        match psbt.unsigned_tx.output.as_mut_slice() {
            // our primary output is the 2nd one? reverse the vectors
            [_, second_txout] if second_txout.script_pubkey == script => {
                psbt.outputs.reverse();
                psbt.unsigned_tx.output.reverse();
            }
            [first_txout, _] if first_txout.script_pubkey == script => {
                // no need to do anything
            }
            [_] => {
                // single output, no need do anything
            }
            _ => bail!("Unexpected transaction layout"),
        }

        if let ([_, change], [_, psbt_output], Some(change_override)) = (
            &mut psbt.unsigned_tx.output.as_mut_slice(),
            &mut psbt.outputs.as_mut_slice(),
            change_override,
        ) {
            tracing::info!(change_override = ?change_override, "Overwriting change address");
            change.script_pubkey = change_override.script_pubkey();
            // Might be populated based on the previously set change address, but for the
            // overwrite we don't know unless we ask the user for more information.
            psbt_output.bip32_derivation.clear();
        }

        Ok(psbt)
    }

    /// Calculates the maximum "giveable" amount of this wallet.
    ///
    /// We define this as the maximum amount we can pay to a single output,
    /// already accounting for the fees we need to spend to get the
    /// transaction confirmed.
    ///
    /// With a limit, estimates a drain of the largest spendable outputs within
    /// that limit. Unselected wallet funds are excluded from the amount, not fees.
    /// Returns a tuple of (max_giveable_amount, spending_fee).
    pub async fn max_giveable(
        &self,
        locking_script_size: usize,
        max_inputs: Option<usize>,
    ) -> Result<(Amount, Amount)> {
        let (max_giveable, fee, _) = self
            .max_giveable_with_weight(locking_script_size, max_inputs)
            .await?;
        Ok((max_giveable, fee))
    }

    /// Same as [`Self::max_giveable`], but also returns the signed weight of
    /// the dummy drain transaction. Callers pricing a specific transaction can
    /// use this instead of a fixed one-input weight.
    pub async fn max_giveable_with_weight(
        &self,
        locking_script_size: usize,
        max_inputs: Option<usize>,
    ) -> Result<(Amount, Amount, Weight)> {
        let mut wallet = self.wallet.lock().await;

        // Construct a dummy drain transaction
        let dummy_script = ScriptBuf::from(vec![0u8; locking_script_size]);

        let mut tx_builder = coin_selection::build_tx(&mut wallet, max_inputs, true)?;

        tx_builder.drain_to(dummy_script.clone());
        tx_builder.fee_absolute(Amount::ZERO);
        tx_builder.drain_wallet();

        // The weight WILL NOT change, even if we change the fee
        // because we are draining the wallet (using all inputs) and
        // always have one output of constant size
        //
        // The only changeable part is the amount of the output.
        // If we increase the fee, the output amount simply will decrease
        //
        // The inputs are constant, so only the output amount changes.
        let (dummy_max_giveable, dummy_weight) = match tx_builder.finish() {
            Ok(psbt) => {
                if psbt.unsigned_tx.output.len() != 1 {
                    bail!("Expected a single output in the dummy transaction");
                }

                let max_giveable = psbt.unsigned_tx.output.first().expect("Expected a single output in the dummy transaction").value;
                let weight = estimated_signed_wallet_weight(&psbt)?;

                Ok((Some(max_giveable), weight))
            },
            Err(bdk_wallet::error::CreateTxError::CoinSelection(_)) => {
                // We don't have enough funds to create a transaction (below dust limit)
                //
                // We still want to to return a valid fee.
                // Callers of this function might want to calculate *how* large
                // the next UTXO needs to be such that we can spend any funds
                //
                // To be able to calculate an accurate fee, we need to figure out
                // the weight our drain transaction if we received another UTXO

                // We create fake deposit UTXO
                // Our dummy drain transaction will spend this deposit UTXO
                let mut fake_deposit_input = bitcoin::psbt::Input::default();

                let dummy_deposit_address = wallet.peek_address(KeychainKind::External, 0);
                let fake_deposit_script = dummy_deposit_address.script_pubkey();
                let fake_deposit_txout = bitcoin::blockdata::transaction::TxOut {
                    // The exact deposit amount does not matter
                    // because we only care about the weight of the transaction
                    // which does not depend on the amount of the input
                    value: DUST_AMOUNT * 5,
                    script_pubkey: fake_deposit_script,
                };
                let fake_deposit_tx = bitcoin::Transaction {
                    version: bitcoin::blockdata::transaction::Version::TWO,
                    lock_time: bitcoin::blockdata::locktime::absolute::LockTime::ZERO,
                    input: vec![bitcoin::TxIn {
                        previous_output: bitcoin::OutPoint::null(), // or some dummy outpoint
                        script_sig: Default::default(),
                        sequence: bitcoin::Sequence::ENABLE_RBF_NO_LOCKTIME,
                        witness: Default::default(),
                    }],
                    output: vec![fake_deposit_txout.clone()],
                };

                let fake_deposit_txid = fake_deposit_tx.compute_txid();

                fake_deposit_input.witness_utxo = Some(fake_deposit_txout);
                fake_deposit_input.non_witness_utxo = Some(fake_deposit_tx);

                // Create outpoint that points to our fake transaction's output 0
                let fake_deposit_outpoint = bitcoin::OutPoint {
                    txid: fake_deposit_txid,
                    vout: 0,
                };

                let satisfaction_weight = wallet
                    .public_descriptor(KeychainKind::External)
                    .max_weight_to_satisfy()
                    .context("Failed to compute the satisfaction weight of our descriptor")?;

                let mut tx_builder = coin_selection::build_tx(&mut wallet, max_inputs, true)?;

                tx_builder.drain_to(dummy_script.clone());
                tx_builder.fee_absolute(Amount::ZERO);
                tx_builder.drain_wallet();

                tx_builder
                    .add_foreign_utxo(
                        fake_deposit_outpoint,
                        fake_deposit_input,
                        satisfaction_weight,
                    ).context("Failed to add dummy foreign utxo to calculate fee for max_giveable if we had one more utxo")?;

                // Try building the dummy drain transaction with the new fake UTXO
                // If we fail now, we propagate the error to the caller
                let psbt = tx_builder.finish()?;
                let weight = estimated_signed_wallet_weight(&psbt)?;

                tracing::trace!(
                    weight = weight.to_wu(),
                    "Built dummy drain transaction with fake UTXO, max giveable is 0"
                );

                Ok((None, weight))
            }
            Err(e) => Err(e)
        }.context("Failed to build transaction to figure out max giveable")?;

        // Estimate the fee rate using our real fee rate estimation
        let fee = self.estimate_fee(dummy_weight, dummy_max_giveable).await?;

        Ok(match dummy_max_giveable {
            // If the max giveable is less than the dust amount, we return 0
            Some(max_giveable) if max_giveable < DUST_AMOUNT => (Amount::ZERO, fee, dummy_weight),
            Some(max_giveable) => {
                // If we have enough funds, we subtract the fee from the max giveable
                // and return the result
                match max_giveable.checked_sub(fee) {
                    Some(max_giveable) => (max_giveable, fee, dummy_weight),
                    // Let's say we have 2000 sats in the wallet
                    // The dummy script chooses 0 sats as a fee
                    // and drains the 2000 sats
                    //
                    // Our smart fee estimation says we need 2500 sats to get the transaction confirmed
                    // fee = 2500
                    // dummy_max_giveable = 2000
                    // max_giveable is < 0, so we return 0 since we don't have enough funds to cover the fee
                    None => (Amount::ZERO, fee, dummy_weight),
                }
            }
            // If we don't know the max giveable, we return 0
            // This happens if we don't have enough funds to create a transaction
            // (below dust limit)
            None => (Amount::ZERO, fee, dummy_weight),
        })
    }

    /// Return an uncapped rate for swap setup, including the current relay minimum.
    pub async fn swap_fee_rate(&self) -> Result<FeeRate> {
        Ok(self
            .combined_fee_rate()
            .await?
            .max(self.combined_min_relay_fee().await?))
    }

    /// Estimate total tx fee for a pre-defined target block based on the
    /// transaction weight. The max fee cannot be more than MAX_PERCENTAGE_FEE
    /// of amount
    ///
    /// This uses different techniques to estimate the fee under the hood:
    /// 1. `estimate_fee_rate` from Electrum which calls `estimatesmartfee` from Bitcoin Core
    /// 2. `estimate_fee_rate_from_histogram` which calls `mempool.get_fee_histogram` from Electrum. It calculates the distance to the tip of the mempool.
    ///    it can adapt faster to sudden spikes in the mempool.
    /// 3. `MempoolClient::estimate_feerate` which uses the mempool.space API for fee estimation
    ///
    /// To compute the min relay fee we fetch from both the Electrum server and the MempoolClient.
    ///
    /// In all cases, if have multiple sources, we use the higher one.
    pub async fn estimate_fee(
        &self,
        weight: Weight,
        transfer_amount: Option<bitcoin::Amount>,
    ) -> Result<bitcoin::Amount> {
        let fee_rate = self.combined_fee_rate().await?;
        let min_relay_fee = self.combined_min_relay_fee().await?;

        estimate_fee(weight, transfer_amount, fee_rate, min_relay_fee)
    }

    /// Estimate the fee for a transaction with `vsize` virtual bytes at the
    /// requested confirmation target.
    pub async fn estimate_fee_vsize_at(
        &self,
        vsize: usize,
        transfer_amount: bitcoin::Amount,
        target_block: u32,
    ) -> Result<bitcoin::Amount> {
        let fee_rate = self.combined_fee_rate_at(target_block).await?;
        let min_relay_fee = self.combined_min_relay_fee().await?;
        estimate_fee(
            vsize_weight(vsize)?,
            Some(transfer_amount),
            fee_rate,
            min_relay_fee,
        )
    }

    /// Like [`Self::estimate_fee_vsize_at`], but multiplies the fee rate
    /// before applying the relay floor and fee caps.
    pub async fn estimate_fee_vsize_at_with_rate_multiplier(
        &self,
        vsize: usize,
        transfer_amount: bitcoin::Amount,
        target_block: u32,
        rate_multiplier: f32,
    ) -> Result<bitcoin::Amount> {
        let fee_rate = self.combined_fee_rate_at(target_block).await?;
        let min_relay_fee = self.combined_min_relay_fee().await?;
        let fee_rate = FeeRate::from_sat_per_vb(
            (fee_rate.to_sat_per_vb_ceil() as f64 * f64::from(rate_multiplier)).ceil() as u64,
        )
        .context("Failed to compute multiplied fee rate")?;

        estimate_fee(
            vsize_weight(vsize)?,
            Some(transfer_amount),
            fee_rate,
            min_relay_fee,
        )
    }

    /// Target-specific weight-based fee estimate.
    pub async fn estimate_fee_at(
        &self,
        weight: usize,
        transfer_amount: bitcoin::Amount,
        target_block: u32,
    ) -> Result<bitcoin::Amount> {
        let fee_rate = self.combined_fee_rate_at(target_block).await?;
        let min_relay_fee = self.combined_min_relay_fee().await?;
        estimate_fee(
            Weight::from_wu(weight as u64),
            Some(transfer_amount),
            fee_rate,
            min_relay_fee,
        )
    }
}

fn vsize_weight(vsize: usize) -> Result<Weight> {
    Weight::from_vb(vsize as u64).context("Failed to convert vsize to weight")
}

/// Wall-clock cap for one balancer attempt (a whole wallet sync chunk).
///
/// A full sync of the swap wallet takes about a minute over Tor even when
/// nothing is wrong (the maker's took 59 s). The balancer's default cap of
/// 60 s sat right on that: a sync needing 65 s was killed and restarted from
/// scratch on the other server, again and again, so "Preparing swap" ran for
/// minutes. Individual stalled requests are still cut off by the 15 s request
/// timeout; this cap only backstops a session that stops responding at all.
const SYNC_CALL_TIMEOUT_SECS: u8 = 150;

impl Client {
    fn balancer_config(socks5_proxy: Option<String>) -> ElectrumBalancerConfig {
        ElectrumBalancerConfig {
            socks5_proxy,
            call_timeout: SYNC_CALL_TIMEOUT_SECS,
            ..Default::default()
        }
    }

    /// Create a new client with multiple electrum servers for load balancing.
    pub async fn new(
        electrum_rpc_urls: &[String],
        sync_interval: Duration,
        socks5_proxy: Option<String>,
        network: Network,
    ) -> Result<Self> {
        let balancer = ElectrumBalancer::new_with_config(
            electrum_rpc_urls.to_vec(),
            Self::balancer_config(socks5_proxy),
        )
        .await?;
        let initial_last_sync = Instant::now()
            .checked_sub(sync_interval)
            .ok_or(anyhow!("failed to set last sync time"))?;

        Ok(Self {
            inner: Arc::new(balancer),
            script_history: Arc::new(TokioRwLock::new(BTreeMap::new())),
            last_sync: Arc::new(SyncMutex::new(initial_last_sync)),
            sync_interval,
            subscription_idle_timeout: DEFAULT_SUBSCRIPTION_IDLE_TIMEOUT,
            latest_block_height: Arc::new(SyncMutex::new(BlockHeight::from(0))),
            subscriptions: Arc::new(TokioMutex::new(HashMap::new())),
            min_header_difficulty: min_header_difficulty(network),
            verified_inclusions: Arc::new(SyncMutex::new(Default::default())),
        })
    }

    /// Update the client state, if the refresh duration has passed.
    ///
    /// Optionally force an update even if the sync interval has not passed.
    pub async fn update_state(&self, force: bool) -> Result<()> {
        let now = Instant::now();

        if !force {
            let last_sync = *self.last_sync.lock().expect("last_sync mutex poisoned");
            if now.duration_since(last_sync) < self.sync_interval {
                return Ok(());
            }
        }

        self.update_script_histories().await?;
        self.update_block_height().await?;

        *self.last_sync.lock().expect("last_sync mutex poisoned") = Instant::now();

        Ok(())
    }

    /// Update the client state for a single script.
    ///
    /// As opposed to [`update_state`] this function does not
    /// check the time since the last update before refreshing
    /// It therefore also does not take a [`force`] parameter
    pub async fn update_state_single(&self, script: &dyn Watchable) -> Result<()> {
        self.update_script_history(script).await?;
        self.update_block_height().await?;

        Ok(())
    }

    /// Update the block height.
    pub async fn update_block_height(&self) -> Result<()> {
        let latest_block = self
            .inner
            .call("block_headers_subscribe", |client| {
                client.inner.block_headers_subscribe()
            })
            .await
            .context("Failed to subscribe to header notifications")?;
        let latest_block_height = BlockHeight::try_from(latest_block)?;

        let mut current = self
            .latest_block_height
            .lock()
            .expect("latest_block_height mutex poisoned");
        if latest_block_height > *current {
            tracing::trace!(
                block_height = u32::from(latest_block_height),
                "Got notification for new block"
            );
            *current = latest_block_height;
        }

        Ok(())
    }

    /// Update the script histories.
    async fn update_script_histories(&self) -> Result<()> {
        let scripts: Vec<_> = self.script_history.read().await.keys().cloned().collect();

        // No need to do any network request if we have nothing to fetch
        if scripts.is_empty() {
            return Ok(());
        }

        // Concurrently fetch the script histories from the electrum servers
        let results = self
            .inner
            .join_quorum("batch_script_get_history", {
                let scripts = scripts.clone();

                move |client| {
                    let script_refs: Vec<_> = scripts.iter().map(|s| s.as_script()).collect();
                    client.inner.batch_script_get_history(script_refs)
                }
            })
            .await?;

        let successful_results: Vec<Vec<Vec<GetHistoryRes>>> = results
            .iter()
            .filter_map(|r| r.as_ref().ok())
            .cloned()
            .collect();

        // If we didn't get a single successful request, we have to fail
        if successful_results.is_empty() {
            if let Some(Err(e)) = results.into_iter().find(|r| r.is_err()) {
                return Err(e.into());
            }
        }

        // Iterate through each script we fetched and find the highest
        // returned entry at any Electrum node
        let mut script_history = self.script_history.write().await;
        for (script_index, script) in scripts.iter().enumerate() {
            let all_history_for_script: Vec<GetHistoryRes> = successful_results
                .iter()
                .filter_map(|server_result| server_result.get(script_index))
                .flatten()
                .cloned()
                .collect();

            let mut best_history: BTreeMap<Txid, GetHistoryRes> = BTreeMap::new();
            for item in all_history_for_script {
                best_history
                    .entry(item.tx_hash)
                    .and_modify(|current| {
                        if item.height > current.height {
                            *current = item.clone();
                        }
                    })
                    .or_insert(item);
            }

            let final_history: Vec<GetHistoryRes> = best_history.into_values().collect();
            script_history.insert(script.clone(), final_history);
        }

        Ok(())
    }

    /// Update the script history of a single script.
    pub async fn update_script_history(&self, script: &dyn Watchable) -> Result<()> {
        let (script_buf, _) = script.script_and_txid();
        let script_clone = script_buf.clone();

        // Call the electrum servers in parallel to get the script history.
        let results = self
            .inner
            .join_quorum("script_get_history", move |client| {
                client.inner.script_get_history(script_clone.as_script())
            })
            .await?;

        // Collect all successful history entries from all servers.
        let mut all_history_items: Vec<GetHistoryRes> = Vec::new();
        let mut any_success = false;
        let mut first_error = None;

        for result in results {
            match result {
                Ok(history) => {
                    any_success = true;
                    all_history_items.extend(history);
                }
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }

        // If any of the calls succeeded, that is fine. Only if none
        // succeeded we return the error.
        if !any_success && let Some(err) = first_error {
            return Err(err.into());
        }

        // Use a map to find the best (highest confirmation) entry for each transaction.
        let mut best_history: BTreeMap<Txid, GetHistoryRes> = BTreeMap::new();
        for item in all_history_items {
            best_history
                .entry(item.tx_hash)
                .and_modify(|current| {
                    if item.height > current.height {
                        *current = item.clone();
                    }
                })
                .or_insert(item);
        }

        let final_history: Vec<GetHistoryRes> = best_history.into_values().collect();

        self.script_history
            .write()
            .await
            .insert(script_buf, final_history);

        Ok(())
    }

    /// Broadcast a transaction to all known electrum servers in parallel.
    /// Returns the results from all servers - at least one success indicates successful broadcast.
    pub async fn transaction_broadcast_all(
        &self,
        transaction: &Transaction,
    ) -> Result<Vec<Result<bitcoin::Txid, bdk_electrum::electrum_client::Error>>> {
        // Broadcast to all electrum servers in parallel
        let results = self.inner.broadcast_all(transaction.clone()).await?;

        // Add the transaction to the cache if at least one broadcast succeeded
        if results.iter().any(|r| r.is_ok()) {
            // Note: Perhaps it is better to only populate caches of the Electrum nodes
            // that accepted our transaction?
            self.inner.populate_tx_cache(vec![transaction.clone()]);
        }

        Ok(results)
    }

    /// Get the status of a script.
    pub async fn status_of_script(
        &self,
        script: &dyn Watchable,
        force: bool,
    ) -> Result<ScriptStatus> {
        let (script_buf, txid) = script.script_and_txid();

        let is_first_time = {
            let mut history = self.script_history.write().await;
            if history.contains_key(&script_buf) {
                false
            } else {
                history.insert(script_buf.clone(), vec![]);
                true
            }
        };

        if is_first_time {
            // Immediately refetch the status of the script
            // when we first subscribe to it.
            self.update_state_single(script).await?;
        } else if force {
            // Immediately refetch the status of the script
            // when [`force`] is set to true
            self.update_state_single(script).await?;
        } else {
            // Otherwise, don't force a refetch.
            self.update_state(false).await?;
        }

        let history_guard = self.script_history.read().await;
        let history = history_guard.get(&script_buf);

        let history_of_tx: Vec<&GetHistoryRes> = history
            .into_iter()
            .flatten()
            .filter(|entry| entry.tx_hash == txid)
            .collect();

        // Destructure history_of_tx into the last entry and the rest.
        let [rest @ .., last] = history_of_tx.as_slice() else {
            // If there is no history of the transaction, it is unseen.
            return Ok(ScriptStatus::Unseen);
        };

        // There should only be one entry per txid, we will ignore the rest
        if !rest.is_empty() {
            tracing::warn!(%txid, "Found multiple history entries for the same txid. Ignoring all but the last one.");
        }

        let claimed_height = last.height;
        drop(history_guard);

        // If the height is 0 or less, the transaction is still in the mempool.
        if claimed_height <= 0 {
            return Ok(ScriptStatus::InMempool);
        }
        let height = u32::try_from(claimed_height)?;

        // A server's history entry is only a claim. Count the transaction as
        // mined only once a merkle proof ties it to a block header with real
        // proof of work (audit H-4).
        match self.verify_inclusion_at(txid, height).await? {
            InclusionVerdict::Proven => {}
            InclusionVerdict::Refuted(reason) => {
                tracing::warn!(
                    %txid,
                    height,
                    %reason,
                    "An Electrum server reported this transaction as confirmed but no server proved it; treating it as unconfirmed"
                );
                return Ok(ScriptStatus::InMempool);
            }
        }

        let latest_block_height = *self
            .latest_block_height
            .lock()
            .expect("latest_block_height mutex poisoned");

        Ok(ScriptStatus::Confirmed(
            Confirmed::from_inclusion_and_latest_block(height, u32::from(latest_block_height)),
        ))
    }

    /// Ask the servers for a merkle proof that `txid` is in the block at
    /// `height`, with that block's header, and check both (audit H-4).
    ///
    /// Each answer proves itself, so one valid answer is enough and a lying
    /// server cannot outvote an honest one. Returns an error only when no
    /// server answered at all, so the caller retries rather than deciding.
    pub async fn verify_inclusion_at(&self, txid: Txid, height: u32) -> Result<InclusionVerdict> {
        if self
            .verified_inclusions
            .lock()
            .expect("verified_inclusions mutex poisoned")
            .contains(&(txid, height))
        {
            return Ok(InclusionVerdict::Proven);
        }

        let results = self
            .inner
            .join_quorum("transaction_get_merkle", move |client| {
                let proof = client.inner.transaction_get_merkle(&txid, height as usize)?;
                let header = client.inner.block_header(height as usize)?;
                Ok((proof, header))
            })
            .await
            .context("Failed to ask the Electrum servers for a merkle proof")?;

        let verdict = judge_inclusion_proofs(&txid, results, self.min_header_difficulty)?;

        if verdict == InclusionVerdict::Proven {
            let mut verified = self
                .verified_inclusions
                .lock()
                .expect("verified_inclusions mutex poisoned");
            // A long-running maker watches a handful of transactions per
            // swap; the bound only stops unlimited growth.
            if verified.len() >= MAX_VERIFIED_INCLUSIONS {
                verified.clear();
            }
            verified.insert((txid, height));
        }

        Ok(verdict)
    }

    /// Get a transaction from the Electrum servers.
    ///
    /// A transaction returned by any single server is taken as proof of its
    /// existence, but only if it hashes to the requested txid: a server that
    /// answers with a different transaction is ignored (audit H-4). It is not
    /// proof that the transaction was mined; [`Client::status_of_script`]
    /// checks that. Concluding that a transaction does *not* exist requires
    /// `min_parallel_responses` servers to independently report it as not
    /// found — or, if fewer servers are reachable, every reachable server (at
    /// least one). If no server gives a valid answer, an error is returned.
    pub async fn get_tx(&self, txid: Txid) -> Result<Option<Arc<Transaction>>> {
        let results = self
            .inner
            .join_quorum("get_raw_transaction", move |client| {
                use bitcoin::consensus::Decodable;

                match client.inner.transaction_get_raw(&txid) {
                    Ok(raw) => {
                        let mut cursor = std::io::Cursor::new(&raw);
                        let tx =
                            bitcoin::Transaction::consensus_decode(&mut cursor).map_err(|e| {
                                bdk_electrum::electrum_client::Error::Protocol(
                                    format!("Failed to deserialize transaction: {}", e).into(),
                                )
                            })?;

                        Ok(Some(tx))
                    }
                    // A recognized "not found" is a valid answer, not a server failure
                    Err(error) if indicates_tx_not_found(&error) => Ok(None),
                    Err(error) => Err(error),
                }
            })
            .await
            .context("Failed to query Electrum servers for transaction")?;

        if let Some(tx) = matching_transaction(&txid, &results) {
            let tx = Arc::new(tx.clone());
            // Note: Perhaps it is better to only populate caches of the Electrum nodes
            // that accepted our transaction?
            self.inner.populate_tx_cache(vec![(*tx).clone()]);
            return Ok(Some(tx));
        }

        let not_found_responses = results
            .iter()
            .filter(|result| matches!(result, Ok(None)))
            .count();
        let required_not_found = self
            .inner
            .config()
            .min_parallel_responses
            .clamp(1, self.inner.client_count());

        // If the quorum was not reached, join_quorum has waited for every server,
        // so the reachable servers' answer is all the evidence there is
        let all_servers_finished = results.len() >= self.inner.client_count();

        if not_found_responses >= required_not_found
            || (all_servers_finished && not_found_responses > 0)
        {
            tracing::trace!(
                txid = %txid,
                not_found_responses,
                required_not_found,
                "Transaction reported as not found by the reachable Electrum servers"
            );
            return Ok(None);
        }

        let errors: Vec<_> = results
            .into_iter()
            .filter_map(|result| result.err())
            .collect();

        Err(anyhow::Error::from(electrum_pool::MultiError::new(
            errors,
            format!(
                "Could not determine whether transaction {} exists: only {} of the required {} servers reported it as not found",
                txid, not_found_responses, required_not_found
            ),
        )))
    }

    /// Estimate the fee rate to be included in a block at the given offset.
    /// Calls: https://electrum-protocol.readthedocs.io/en/latest/protocol-methods.html#blockchain.estimatefee
    /// Calls under the hood: https://developer.bitcoin.org/reference/rpc/estimatesmartfee.html
    ///
    /// This uses estimatesmartfee of bitcoind
    pub async fn estimate_fee_rate(&self, target_block: u32) -> Result<FeeRate> {
        // Get the fee rate in Bitcoin per kilobyte
        let btc_per_kvb = self
            .inner
            .call("estimate_fee", move |client| {
                client.inner.estimate_fee(target_block as usize)
            })
            .await?;

        // If the fee rate is less than 0, return an error
        // The Electrum server returns a value <= 0 if it cannot estimate the fee rate.
        // See: https://github.com/romanz/electrs/blob/ed0ef2ee22efb45fcf0c7f3876fd746913008de3/src/electrum.rs#L239-L245
        //      https://github.com/romanz/electrs/blob/ed0ef2ee22efb45fcf0c7f3876fd746913008de3/src/electrum.rs#L31
        if btc_per_kvb <= 0.0 {
            return Err(anyhow!(
                "Fee rate returned by Electrum server is less than 0"
            ));
        }

        // Convert to sat / kB without ever constructing an Amount from the float
        // Simply by multiplying the float with the satoshi value of 1 BTC.
        // Truncation is allowed here because we are converting to sats and rounding down sats will
        // not lose us any precision (because there is no fractional satoshi).
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let sats_per_kvb = (btc_per_kvb * Amount::ONE_BTC.to_sat() as f64).ceil() as u64;

        // Convert to sat / kwu (kwu = kB × 4)
        let sat_per_kwu = sats_per_kvb / 4;

        // Construct the fee rate
        let fee_rate = FeeRate::from_sat_per_kwu(sat_per_kwu);

        Ok(fee_rate)
    }

    /// Calculates the fee_rate needed to be included in a block at the given offset.
    /// We calculate how many vMB we are away from the tip of the mempool.
    /// This method adapts faster to sudden spikes in the mempool.
    async fn estimate_fee_rate_from_histogram(&self, target_block: u32) -> Result<FeeRate> {
        // Assume we want to get into the next block:
        // We want to be 80% of the block size away from the tip of the mempool.
        const HISTOGRAM_SAFETY_MARGIN: f32 = 0.8;

        // First we fetch the fee histogram from the Electrum server
        let fee_histogram = self
            .inner
            .call("get_fee_histogram", move |client| {
                client.inner.raw_call("mempool.get_fee_histogram", vec![])
            })
            .await?;

        // Parse the histogram as array of [fee, vsize] pairs
        let histogram: Vec<(f64, u64)> = serde_json::from_value(fee_histogram)?;

        // If the histogram is empty, we return an error
        if histogram.is_empty() {
            return Err(anyhow!(
                "The mempool seems to be empty therefore we cannot estimate the fee rate from the histogram"
            ));
        }

        // Sort the histogram by fee rate
        let mut histogram = histogram;
        histogram.sort_by(|(a, _), (b, _)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        // Estimate block size (typically ~1MB = 1,000,000 vbytes)
        let estimated_block_size = 1_000_000u64;
        #[allow(clippy::cast_precision_loss)]
        let target_distance_from_tip =
            (estimated_block_size * target_block as u64) as f32 * HISTOGRAM_SAFETY_MARGIN;

        // Find cumulative vsize and corresponding fee rate
        let mut cumulative_vsize = 0u64;
        for (fee_rate, vsize) in histogram.clone() {
            cumulative_vsize += vsize;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            if cumulative_vsize >= target_distance_from_tip as u64 {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let sat_per_vb = fee_rate.ceil() as u64;
                return FeeRate::from_sat_per_vb(sat_per_vb)
                    .context("Failed to create fee rate from histogram");
            }
        }

        // If we get here, the entire mempool is less than the target distance from the tip.
        // We return the lowest fee rate in the histogram.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let sat_per_vb = histogram
            .first()
            .expect("The histogram should not be empty")
            .0
            .ceil() as u64;
        FeeRate::from_sat_per_vb(sat_per_vb)
            .context("Failed to create fee rate from histogram (all mempool is less than the target distance from the tip)")
    }

    /// Get the minimum relay fee rate from the Electrum server.
    async fn min_relay_fee(&self) -> Result<FeeRate> {
        let min_relay_btc_per_kvb = self
            .inner
            .call("relay_fee", |client| client.inner.relay_fee())
            .await?;

        // Convert to sat / kB without ever constructing an Amount from the float
        // Simply by multiplying the float with the satoshi value of 1 BTC.
        // Truncation is allowed here because we are converting to sats and rounding down sats will
        // not lose us any precision (because there is no fractional satoshi).
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let sats_per_kvb = (min_relay_btc_per_kvb * Amount::ONE_BTC.to_sat() as f64).ceil() as u64;

        // Convert to sat / kwu (kwu = kB × 4)
        let sat_per_kwu = sats_per_kvb / 4;

        // Construct the fee rate
        let fee_rate = FeeRate::from_sat_per_kwu(sat_per_kwu);

        Ok(fee_rate)
    }
}

/// Whether the lock input check was cancelled, or its canceller is gone.
fn is_cancelled(cancel: &watch::Receiver<bool>) -> bool {
    *cancel.borrow() || cancel.has_changed().is_err()
}

/// Returns true if the error is a server response indicating that the
/// requested transaction does not exist (as opposed to the server failing
/// to answer the query).
fn indicates_tx_not_found(error: &bdk_electrum::electrum_client::Error) -> bool {
    let error_str = error.to_string();

    if error_str.contains("\"code\": Number(-5)")
        || error_str.contains("No such mempool or blockchain transaction")
        || error_str.contains("missing transaction")
    {
        return true;
    }

    let err_anyhow = anyhow!(error_str);
    matches!(
        parse_rpc_error_code(&err_anyhow),
        Ok(code) if code == i64::from(RpcErrorCode::RpcInvalidAddressOrKey)
    )
}

/// Extracts the account xpub from a descriptor string such as
/// `wpkh([ea8fea5b/84'/1'/2147483643']tpubDD…/0/*)#checksum`.
fn account_xpub(descriptor: &str) -> Option<String> {
    let start = descriptor
        .find("tpub")
        .or_else(|| descriptor.find("xpub"))?;
    let rest = &descriptor[start..];
    let end = rest
        .find(|character: char| !character.is_ascii_alphanumeric())
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone)]
pub struct SyncRequestBuilderFactory {
    chain_tip: bdk_wallet::chain::CheckPoint,
    spks: Vec<((KeychainKind, u32), ScriptBuf)>,
}

impl SyncRequestBuilderFactory {
    fn build(self) -> SyncRequestBuilder<(KeychainKind, u32)> {
        SyncRequest::builder()
            .chain_tip(self.chain_tip)
            .spks_with_indexes(self.spks)
    }
}

#[async_trait::async_trait]
impl VerifyPsbtPrevouts for Wallet {
    async fn verify_psbt_prevouts(
        &self,
        psbt: &Psbt,
        cancel: &watch::Receiver<bool>,
    ) -> std::result::Result<(), PsbtPrevoutVerificationError> {
        Wallet::verify_psbt_prevouts(self, psbt, cancel).await
    }
}

#[async_trait::async_trait]
impl BitcoinWallet for Wallet {
    async fn balance(&self) -> Result<Amount> {
        Wallet::balance(self).await
    }

    async fn balance_info(&self) -> Result<Balance> {
        Wallet::balance_info(self).await
    }

    async fn new_address(&self) -> Result<Address> {
        Wallet::new_address(self).await
    }

    async fn send_to_address(
        &self,
        address: Address,
        amount: Amount,
        spending_fee: Amount,
        change_override: Option<Address>,
        max_inputs: Option<usize>,
    ) -> Result<Psbt> {
        Wallet::send_to_address(
            self,
            address,
            amount,
            spending_fee,
            change_override,
            max_inputs,
        )
        .await
    }

    async fn send_to_address_dynamic_fee(
        &self,
        address: Address,
        amount: Amount,
        change_override: Option<Address>,
        max_inputs: Option<usize>,
    ) -> Result<Psbt> {
        Wallet::send_to_address_dynamic_fee(self, address, amount, change_override, max_inputs)
            .await
    }

    async fn sweep_balance_to_address_dynamic_fee(&self, address: Address) -> Result<Psbt> {
        Wallet::sweep_balance_to_address_dynamic_fee(self, address).await
    }

    async fn sign_and_finalize(&self, psbt: Psbt) -> Result<bitcoin::Transaction> {
        Wallet::sign_and_finalize(self, psbt).await
    }

    fn prove_utxo_ownership(
        &self,
        psbt: &Psbt,
        challenge: utxo_ownership_proof::Challenge,
    ) -> Result<Vec<utxo_ownership_proof::OwnershipProof>> {
        Wallet::prove_utxo_ownership(self, psbt, challenge)
    }

    async fn ensure_broadcasted(
        &self,
        tx: bitcoin::Transaction,
        kind: &str,
    ) -> Result<(Txid, Subscription)> {
        Wallet::ensure_broadcasted(self, tx, kind).await
    }

    async fn sync(&self) -> Result<()> {
        Wallet::sync(self).await
    }

    async fn health_check(&self) -> Result<()> {
        Wallet::health_check(self).await
    }

    async fn subscribe_to(&self, tx: Box<dyn Watchable>) -> Subscription {
        Wallet::subscribe_to(self, tx).await
    }

    async fn status_of_script(&self, tx: &dyn Watchable) -> Result<ScriptStatus> {
        Wallet::status_of_script(self, tx).await
    }

    async fn get_raw_transaction(&self, txid: Txid) -> Result<Option<std::sync::Arc<Transaction>>> {
        Wallet::get_raw_transaction(self, txid).await
    }

    async fn max_giveable(
        &self,
        locking_script_size: usize,
        max_inputs: Option<usize>,
    ) -> Result<(Amount, Amount)> {
        Wallet::max_giveable(self, locking_script_size, max_inputs).await
    }

    async fn estimate_fee(
        &self,
        weight: Weight,
        transfer_amount: Option<Amount>,
    ) -> Result<Amount> {
        Wallet::estimate_fee(self, weight, transfer_amount).await
    }

    async fn swap_fee_rate(&self) -> Result<FeeRate> {
        Wallet::swap_fee_rate(self).await
    }

    fn network(&self) -> Network {
        self.network
    }

    fn finality_confirmations(&self) -> u32 {
        self.finality_confirmations
    }

    async fn wallet_export(&self, role: &str) -> Result<FullyNodedExport> {
        Wallet::wallet_export(self, role).await
    }
}

impl EstimateFeeRate for Client {
    async fn estimate_feerate(&self, target_block: u32) -> Result<FeeRate> {
        // Now that the Electrum client methods are async, we can parallelize the calls
        let (electrum_conservative_fee_rate, electrum_histogram_fee_rate) = tokio::join!(
            self.estimate_fee_rate(target_block),
            self.estimate_fee_rate_from_histogram(target_block)
        );

        match (electrum_conservative_fee_rate, electrum_histogram_fee_rate) {
            // If both the histogram and conservative fee rate are successful, we use the higher one
            (Ok(electrum_conservative_fee_rate), Ok(electrum_histogram_fee_rate)) => {
                tracing::debug!(
                    electrum_conservative_fee_rate_sat_vb =
                        electrum_conservative_fee_rate.to_sat_per_vb_ceil(),
                    electrum_histogram_fee_rate_sat_vb =
                        electrum_histogram_fee_rate.to_sat_per_vb_ceil(),
                    "Successfully fetched fee rates from both sources. We will use the higher one"
                );

                Ok(electrum_conservative_fee_rate.max(electrum_histogram_fee_rate))
            }
            // If the conservative fee rate fails, we use the histogram fee rate
            (Err(electrum_conservative_fee_rate_error), Ok(electrum_histogram_fee_rate)) => {
                tracing::warn!(
                    electrum_conservative_fee_rate_error = ?electrum_conservative_fee_rate_error,
                    electrum_histogram_fee_rate_sat_vb = electrum_histogram_fee_rate.to_sat_per_vb_ceil(),
                    "Failed to fetch conservative fee rate, using histogram fee rate"
                );
                Ok(electrum_histogram_fee_rate)
            }
            // If the histogram fee rate fails, we use the conservative fee rate
            (Ok(electrum_conservative_fee_rate), Err(electrum_histogram_fee_rate_error)) => {
                tracing::warn!(
                    electrum_histogram_fee_rate_error = ?electrum_histogram_fee_rate_error,
                    electrum_conservative_fee_rate_sat_vb = electrum_conservative_fee_rate.to_sat_per_vb_ceil(),
                    "Failed to fetch histogram fee rate, using conservative fee rate"
                );
                Ok(electrum_conservative_fee_rate)
            }
            // If both the histogram and conservative fee rate fail, we return an error
            (Err(electrum_conservative_fee_rate_error), Err(electrum_histogram_fee_rate_error)) => {
                Err(electrum_conservative_fee_rate_error
                    .context(electrum_histogram_fee_rate_error)
                    .context("Failed to fetch both the conservative and histogram fee rates from Electrum"))
            }
        }
    }

    async fn min_relay_fee(&self) -> Result<FeeRate> {
        Client::min_relay_fee(self).await
    }
}

/// Extension trait for our custom concurrent sync implementation.
mod sync_ext {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::Mutex as SyncMutex;

    use bdk_wallet::KeychainKind;

    use super::IntoArcMutex;

    /// Type alias for an optional callback
    /// that is used to report progress of a sync (or a chunk of a sync)
    pub type InnerSyncCallback = Option<Box<dyn FnMut(u64, u64) + Send + 'static>>;

    /// Type alias for the thread-safe, reference-counted callback of an [`InnerSyncCallback`]
    pub type SyncCallback = Arc<SyncMutex<InnerSyncCallback>>;

    pub trait SyncCallbackExt {
        #[allow(clippy::new_ret_no_self)]
        fn new<F>(callback: F) -> InnerSyncCallback
        where
            F: FnMut(u64, u64) + Send + 'static;
        fn throttle_callback(self, min_percentage_increase: f32) -> InnerSyncCallback;
        fn chain(self, callback: InnerSyncCallback) -> InnerSyncCallback;
        fn finalize(self) -> SyncCallback;
        fn call(&mut self, consumed: u64, total: u64);
        #[allow(clippy::type_complexity)]
        fn to_full_scan_callback(
            self,
            stop_gap: u32,
            assumed_buffer: u32,
        ) -> Box<dyn FnMut(KeychainKind, u32, &bitcoin::Script) + Send + 'static>
        where
            Self: Sized;
    }

    impl SyncCallbackExt for InnerSyncCallback {
        /// Creates a new sync callback from a callback function.
        fn new<F>(callback: F) -> InnerSyncCallback
        where
            F: FnMut(u64, u64) + Send + 'static,
        {
            Some(Box::new(callback))
        }

        /// Throttles a sync callback, invoking the original callback only when
        /// the progress has increased by at least `min_percentage_increase` since the last invocation.
        ///
        /// Ensures the callback is always invoked when progress reaches 100%.
        fn throttle_callback(self, min_percentage_increase: f32) -> InnerSyncCallback {
            let mut callback = self?;

            let mut last_reported_percentage: f64 = 0.0;
            let threshold = min_percentage_increase as f64 / 100.0;
            let threshold = threshold.clamp(0.0, 1.0);

            #[allow(clippy::cast_precision_loss)]
            Some(Box::new(move |consumed, total| {
                if total == 0 {
                    return;
                }

                let current_percentage = consumed as f64 / total as f64;
                let is_complete = consumed == total;
                let should_report = is_complete
                    || (current_percentage - last_reported_percentage >= threshold)
                    || last_reported_percentage == 0.0;

                if should_report {
                    callback(consumed, total);
                    last_reported_percentage = current_percentage;
                }
            }))
        }

        /// Chains this callback with another callback
        /// Creates a new callback that invokes both callbacks in order.
        fn chain(mut self, mut callback: InnerSyncCallback) -> InnerSyncCallback {
            Self::new(move |consumed, total| {
                self.call(consumed, total);
                callback.call(consumed, total);
            })
        }

        /// Calls the callback with the given progress, if it's Some(...).
        fn call(&mut self, consumed: u64, total: u64) {
            if let Some(cb) = self.as_mut() {
                cb(consumed, total);
            }
        }

        /// Builds a Arc<Mutex<Self>> from the callback
        fn finalize(self) -> SyncCallback {
            self.into_arc_mutex_sync()
        }

        fn to_full_scan_callback(
            mut self,
            stop_gap: u32,
            assumed_buffer: u32,
        ) -> Box<dyn FnMut(KeychainKind, u32, &bitcoin::Script) + Send + 'static>
        where
            Self: Sized,
        {
            Box::new(move |_, current_index, _| {
                let total = stop_gap.max(current_index + assumed_buffer);

                self.call(current_index as u64, total as u64);
            })
        }
    }

    // This struct combines progress updates from different chunks
    // and makes them seem like a single progress update to outsiders
    pub struct CumulativeProgress(HashMap<u64, (u64, u64)>);

    impl CumulativeProgress {
        pub fn new() -> Self {
            Self(HashMap::new())
        }

        /// Get the cumulative progress from all cached singular progress updates
        pub fn get_cumulative(&self) -> (u64, u64) {
            let total_consumed = self.0.values().map(|(consumed, _)| *consumed).sum();
            let total_total = self.0.values().map(|(_, total)| *total).sum();

            (total_consumed, total_total)
        }

        /// Updates the progress of a single chunk
        pub fn insert_single(&mut self, index: u64, consumed: u64, total: u64) {
            self.0.insert(index, (consumed, total));
        }
    }

    pub trait CumulativeProgressHandle {
        fn chunk_callback(self, callback: SyncCallback, index: u64) -> InnerSyncCallback;
    }

    impl CumulativeProgressHandle for Arc<SyncMutex<CumulativeProgress>> {
        /// Takes a callback function and an index of singular SyncRequest chunk
        ///
        /// Returns a new SyncCallback that when called will update the cumulative progress
        ///
        /// The given callback will be called when there's a progress update for the given chunk
        ///
        /// If one wants to a callback to be called called for every update you need to
        /// pass it into every call to this function for every chunk.
        fn chunk_callback(self, callback: SyncCallback, index: u64) -> InnerSyncCallback {
            InnerSyncCallback::new(move |consumed, total| {
                // Insert the latest progress update into the cache
                if let Ok(mut cache) = self.lock() {
                    cache.insert_single(index, consumed, total);

                    // Calculate the cumulative consumed and the cumulative total
                    let (cumulative_consumed, cumulative_total) = cache.get_cumulative();

                    // Send the cumulative progress to the callback
                    // We use sync Mutex here but it's ok because we're only blocking for a short time
                    let callback = callback.lock();
                    if let Ok(mut callback) = callback {
                        callback.call(cumulative_consumed, cumulative_total);
                    }
                }
            })
        }
    }
}

pub fn trace_status_change(
    txid: Txid,
    old: Option<ScriptStatus>,
    new: ScriptStatus,
) -> ScriptStatus {
    match (old, new) {
        (None, new_status) => {
            tracing::debug!(%txid, status = %new_status, "Found relevant Bitcoin transaction");
        }
        (Some(old_status), new_status) if old_status != new_status => {
            tracing::trace!(%txid, %new_status, %old_status, "Bitcoin transaction status changed");
        }
        _ => {}
    }

    new
}

/// Upper bound on the weight of `tx` once signed, rounded up to whole virtual
/// bytes. Assumes every input spends a P2WPKH output and is signed with a
/// maximum-size signature.
pub fn estimated_signed_p2wpkh_weight(tx: &Transaction) -> Weight {
    let weight = predict_weight(
        tx.input.iter().map(|_| InputWeightPrediction::P2WPKH_MAX),
        tx.script_pubkey_lens(),
    );
    Weight::from_vb_unchecked(weight.to_vbytes_ceil())
}

/// Weight of a transaction built by our wallet once we sign every input.
fn estimated_signed_wallet_weight(psbt: &Psbt) -> Result<Weight> {
    for input in &psbt.inputs {
        let utxo = input
            .witness_utxo
            .as_ref()
            .context("Wallet transaction input has no witness UTXO")?;
        anyhow::ensure!(
            utxo.script_pubkey.is_p2wpkh(),
            "Wallet transaction input does not spend a P2WPKH output"
        );
    }
    Ok(estimated_signed_p2wpkh_weight(&psbt.unsigned_tx))
}

/// Estimate the absolute fee for a transaction.
///
/// This function takes the following parameters:
/// - `weight`: The weight of the transaction
/// - `transfer_amount`: The amount of the transfer. Can be `None` if we don't know the transfer amount yet.
///    If the transfer amount is `None`, we will not check the relative fee bound.
/// - `fee_rate_estimation`: The fee rate provided by the user (from fee estimation source)
/// - `min_relay_fee_rate`: The minimum relay fee rate (from fee estimation source, might vary depending on mempool congestion)
///
/// This function will fail if:
/// - The transfer amount is less than the dust amount
/// - The fee rate / min relay fee rate provided by the user is greater than 100M sat/vbyte (sanity check)
///
/// This functions ensures:
/// - We never spend more than MAX_RELATIVE_TX_FEE of the transfer amount on fees
/// - We never use a fee rate higher than MAX_TX_FEE_RATE (100M sat/vbyte)
/// - We never go below 1000 sats (absolute minimum relay fee)
/// - We never go below the minimum relay fee rate (from the fee estimation source)
///
/// We also add a constant safety margin to the fee
pub fn estimate_fee(
    weight: Weight,
    transfer_amount: Option<Amount>,
    fee_rate_estimation: FeeRate,
    min_relay_fee_rate: FeeRate,
) -> Result<Amount> {
    if let Some(transfer_amount) = transfer_amount {
        // We cannot transfer less than the dust amount
        if transfer_amount <= DUST_AMOUNT {
            bail!(
                "Transfer amount needs to be greater than Bitcoin dust amount. Got: {} sats",
                transfer_amount.to_sat()
            );
        }
    }

    // Sanity checks
    if fee_rate_estimation.to_sat_per_vb_ceil() > 100_000_000
        || min_relay_fee_rate.to_sat_per_vb_ceil() > 100_000_000
    {
        bail!("A fee_rate or min_relay_fee of > 1BTC does not make sense")
    }

    // Choose the highest fee rate of:
    // 1. The fee rate provided by the user (comes from fee estimation source)
    // 2. The minimum relay fee rate (comes from fee estimation source, might vary depending on mempool congestion)
    // 3. The broadcast minimum fee rate (hardcoded in the Bitcoin library)
    // We round up to the next sat/vbyte
    let recommended_fee_rate = FeeRate::from_sat_per_vb(
        fee_rate_estimation
            .to_sat_per_vb_ceil()
            .max(min_relay_fee_rate.to_sat_per_vb_ceil())
            .max(FeeRate::BROADCAST_MIN.to_sat_per_vb_ceil()),
    )
    .context("Failed to compute recommended fee rate")?;

    if recommended_fee_rate > fee_rate_estimation {
        tracing::warn!(
            "Estimated fee was below the minimum relay fee rate. Falling back to: {} sats/vbyte",
            recommended_fee_rate.to_sat_per_vb_ceil()
        );
    }

    // Compute the absolute fee in satoshis for the given weight
    let recommended_fee_absolute_sats = recommended_fee_rate
        .checked_mul_by_weight(weight)
        .context("Failed to compute recommended fee rate")?;

    tracing::debug!(
        ?transfer_amount,
        %weight,
        %fee_rate_estimation,
        recommended_fee_rate = %recommended_fee_rate.to_sat_per_vb_ceil(),
        %recommended_fee_absolute_sats,
        "Estimated fee for transaction",
    );

    // If the recommended fee is above the absolute max allowed fee, we fall back to the absolute max allowed fee
    //
    // We only care about this if the transfer amount is known
    if let Some(transfer_amount) = transfer_amount {
        // We never want to spend more than specific percentage of the transfer amount
        // on fees
        let absolute_max_allowed_fee = Amount::from_sat(
            MAX_RELATIVE_TX_FEE
                .saturating_mul(Decimal::from(transfer_amount.to_sat()))
                .ceil()
                .to_u64()
                .expect("Max relative tx fee to fit into u64"),
        );

        if recommended_fee_absolute_sats > absolute_max_allowed_fee {
            let max_relative_tx_fee_percentage = MAX_RELATIVE_TX_FEE
                .saturating_mul(Decimal::from(100))
                .ceil()
                .to_u64()
                .expect("Max relative tx fee to fit into u64");

            tracing::warn!(
                "Relative bound of transaction fees reached. We don't want to spend more than {}% of our transfer amount on fees. Falling back to: {} sats",
                max_relative_tx_fee_percentage,
                absolute_max_allowed_fee.to_sat()
            );

            return Ok(absolute_max_allowed_fee);
        }
    }

    // Bitcoin Core has a minimum relay fee of 1000 sats, regardless of the transaction size
    // Essentially this is an extension of the minimum relay fee rate
    // but some nodes ceil the transaction size to 1000 vbytes
    if recommended_fee_absolute_sats < MIN_ABSOLUTE_TX_FEE {
        // debug, not warn: on a quiet chain this fires on every call (every second while a
        // deposit is awaited), and swap fees do not use this path (docs/FEES.md)
        tracing::debug!(
            "Recommended fee rate is below the absolute minimum relay fee. Falling back to: {} sats",
            MIN_ABSOLUTE_TX_FEE.to_sat()
        );

        return Ok(MIN_ABSOLUTE_TX_FEE);
    }

    // We have a hard limit of 100M sats on the absolute fee
    if recommended_fee_absolute_sats > MAX_ABSOLUTE_TX_FEE {
        tracing::warn!(
            "Hard bound of transaction fee reached. Falling back to: {} sats",
            MAX_ABSOLUTE_TX_FEE.to_sat()
        );

        return Ok(MAX_ABSOLUTE_TX_FEE);
    }

    // Return the recommended fee without any safety margin
    Ok(recommended_fee_absolute_sats)
}

#[allow(dead_code)]
mod mempool_client {
    static HTTP_TIMEOUT: Duration = Duration::from_secs(15);
    static BASE_URL: &str = "https://mempool.space";

    use super::EstimateFeeRate;
    use anyhow::{Context, Result, bail};
    use bitcoin::{FeeRate, Network};
    use serde::Deserialize;
    use std::time::Duration;

    /// A client for the mempool.space API.
    ///
    /// This client is used to estimate the fee rate for a transaction.
    #[derive(Clone)]
    pub struct MempoolClient {
        client: reqwest::Client,
        base_url: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct MempoolFees {
        fastest_fee: u64,
        half_hour_fee: u64,
        hour_fee: u64,
        minimum_fee: u64,
    }

    #[allow(dead_code)]
    impl MempoolClient {
        /// Refuses to build: this client uses a plain, unproxied HTTP client, and Outpost
        /// never calls clearnet fee APIs (review T-5). The wallet builders turn the error into
        /// "no mempool.space client" and fall back to Electrum/Dojo, so flipping
        /// `use_mempool_space_fee_estimation` on cannot leak requests off Tor.
        pub fn new(_network: Network) -> Result<Self> {
            bail!("mempool.space fee estimation is disabled: Outpost never calls clearnet fee APIs")
        }

        /// Fetch the fees (`fees/recommended` endpoint) from the mempool.space API
        async fn fetch_fees(&self) -> Result<MempoolFees> {
            let url = format!("{}/api/v1/fees/recommended", self.base_url);

            let response = self.client.get(url).send().await?;

            let fees: MempoolFees = response.json().await?;

            Ok(fees)
        }
    }

    impl EstimateFeeRate for MempoolClient {
        async fn estimate_feerate(&self, target_block: u32) -> Result<FeeRate> {
            let fees = self.fetch_fees().await?;

            // Match the target block to the correct fee rate
            let sat_per_vb = match target_block {
                0..=2 => fees.fastest_fee,
                3 => fees.half_hour_fee,
                _ => fees.hour_fee,
            };

            // Construct the fee rate
            FeeRate::from_sat_per_vb(sat_per_vb)
                .context("Failed to parse mempool fee rate (out of range)")
        }

        async fn min_relay_fee(&self) -> Result<FeeRate> {
            let fees = self.fetch_fees().await?;

            // Match the target block to the correct fee rate
            let minimum_relay_fee = fees.minimum_fee;

            // Construct the fee rate
            FeeRate::from_sat_per_vb(minimum_relay_fee)
                .context("Failed to parse mempool min relay fee (out of range)")
        }
    }
}

/// Check transaction history before broadcasting. If the initial broadcast
/// fails, query history again to cover the case where the server accepted it
/// but lost the response. Never treat a failed history query as proof of absence.
async fn broadcast_if_unknown<S, SFut, B, BFut>(
    txid: Txid,
    script: ScriptBuf,
    mut status: S,
    broadcast: B,
) -> Result<bool>
where
    S: FnMut((Txid, ScriptBuf)) -> SFut,
    SFut: std::future::Future<Output = Result<ScriptStatus>>,
    B: FnOnce() -> BFut,
    BFut: std::future::Future<Output = Result<()>>,
{
    match status((txid, script.clone())).await? {
        ScriptStatus::InMempool | ScriptStatus::Confirmed(_) => return Ok(false),
        ScriptStatus::Retrying => {
            bail!("Electrum could not determine transaction status; defer broadcast")
        }
        ScriptStatus::Unseen => {}
    }

    match broadcast().await {
        Ok(()) => Ok(true),
        Err(broadcast_error) => match status((txid, script)).await {
            Ok(status) if status.has_been_seen() => Ok(false),
            Ok(ScriptStatus::Retrying) => Err(broadcast_error.context(format!(
                "Could not determine whether transaction {} reached Electrum; keep the signed transaction for retry",
                txid
            ))),
            Ok(_) => Err(broadcast_error),
            Err(status_error) => Err(broadcast_error.context(format!(
                "Could not confirm whether transaction {} reached Electrum: {:#}",
                txid, status_error
            ))),
        },
    }
}

/// Tries the Dojo broadcast up to `attempts` times, with `retry_delay` between
/// failures, before the caller falls back to Electrum.
async fn broadcast_via_dojo<F, Fut>(
    txid: Txid,
    kind: &str,
    attempts: u32,
    retry_delay: Duration,
    mut attempt_broadcast: F,
) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    for attempt in 1..=attempts {
        let attempt_started = std::time::Instant::now();
        match attempt_broadcast().await {
            Ok(()) => {
                tracing::info!(
                    %txid,
                    %kind,
                    %attempt,
                    elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                    "Published Bitcoin transaction via Dojo"
                );
                return true;
            }
            Err(error) => {
                tracing::warn!(
                    %txid,
                    %kind,
                    %attempt,
                    attempts,
                    elapsed_ms = attempt_started.elapsed().as_millis() as u64,
                    "Dojo broadcast attempt failed: {:#}",
                    error
                );
            }
        }

        if attempt < attempts {
            tracing::info!(
                %txid,
                %kind,
                %attempt,
                "Retrying Bitcoin broadcast through Dojo before falling back to Electrum"
            );
            tokio::time::sleep(retry_delay).await;
        }
    }

    false
}

pub mod pre_1_0_0_bdk {
    //! This module contains some code for creating a bdk wallet from before the update.
    //! We need to keep this around to be able to migrate the wallet.

    use std::path::Path;
    use std::sync::Arc;

    use anyhow::{Result, anyhow};
    use bdk::KeychainKind;
    use bdk::bitcoin::Network;
    use bdk::bitcoin::util::bip32::{ChildNumber, DerivationPath, ExtendedPrivKey};
    use bdk::descriptor::{DescriptorError, Segwitv0};
    use bdk::keys::DerivableKey;
    use bdk::sled::Tree;
    use bdk::template::{DescriptorTemplate, DescriptorTemplateOut, P2Wpkh};
    use tokio::sync::Mutex as TokioMutex;

    use super::{IntoArcMutex, WalletAccount};

    pub const WALLET: &str = "wallet";
    const SLED_TREE_NAME: &str = "default_tree";

    #[derive(Debug, Clone)]
    struct SamouraiBip84Legacy<K: DerivableKey<Segwitv0>>(K, KeychainKind, WalletAccount);

    impl<K: DerivableKey<Segwitv0>> DescriptorTemplate for SamouraiBip84Legacy<K> {
        fn build(self, network: Network) -> Result<DescriptorTemplateOut, DescriptorError> {
            let coin_type = match network {
                Network::Bitcoin => 0,
                _ => 1,
            };
            let keychain = match self.1 {
                KeychainKind::External => 0,
                KeychainKind::Internal => 1,
            };
            let path = DerivationPath::from(vec![
                ChildNumber::from_hardened_idx(84).expect("84 is a valid hardened index"),
                ChildNumber::from_hardened_idx(coin_type)
                    .expect("coin type is a valid hardened index"),
                ChildNumber::from_hardened_idx(self.2.account_number())
                    .expect("account number is a valid hardened index"),
                ChildNumber::from_normal_idx(keychain).expect("keychain is a valid normal index"),
            ]);

            P2Wpkh((self.0, path)).build(network)
        }
    }

    /// The is the old bdk wallet before the migration.
    /// We need to construct it before migration to get the keys and revelation indices.
    pub struct OldWallet<D = Tree> {
        wallet: Arc<TokioMutex<bdk::Wallet<D>>>,
        network: Network,
    }

    /// This is all the data we need from the old wallet to be able to migrate it
    /// and check whether we did it correctly.
    pub struct Export {
        /// Wallet descriptor and blockheight.
        pub export: bdk_wallet::export::FullyNodedExport,
        /// Index of the last external address that was revealed.
        pub external_derivation_index: u32,
        /// Index of the last internal address that was revealed.
        pub internal_derivation_index: u32,
    }

    impl OldWallet {
        /// Create a new old wallet.
        pub async fn new(
            data_dir: impl AsRef<Path>,
            xprivkey: ExtendedPrivKey,
            network: Network,
            account: WalletAccount,
        ) -> Result<Self> {
            let data_dir = data_dir.as_ref();
            let wallet_dir = data_dir.join(WALLET);
            let database = bdk::sled::open(wallet_dir)?.open_tree(SLED_TREE_NAME)?;

            let wallet = bdk::Wallet::new(
                SamouraiBip84Legacy(xprivkey, KeychainKind::External, account),
                Some(SamouraiBip84Legacy(
                    xprivkey,
                    KeychainKind::Internal,
                    account,
                )),
                network,
                database,
            )?;

            Ok(Self {
                wallet: wallet.into_arc_mutex_async(),
                network,
            })
        }

        /// Get a full export of the wallet including descriptors and blockheight.
        /// It also includes the internal (change) address and external (receiving) address derivation indices.
        pub async fn export(&self, role: &str) -> Result<Export> {
            let wallet = self.wallet.lock().await;
            let export = bdk::wallet::export::FullyNodedExport::export_wallet(
                &wallet,
                &format!("{}-{}", role, self.network),
                true,
            )
            .map_err(|_| anyhow!("Failed to export old wallet descriptor"))?;

            // Because we upgraded bdk, the type id changed.
            // Thus, we serialize to json and then deserialize to the new type.
            let json = serde_json::to_string(&export)?;
            let export = serde_json::from_str::<bdk_wallet::export::FullyNodedExport>(&json)?;

            let external_info = wallet.get_address(bdk::wallet::AddressIndex::LastUnused)?;
            let external_derivation_index = external_info.index;

            let internal_info =
                wallet.get_internal_address(bdk::wallet::AddressIndex::LastUnused)?;
            let internal_derivation_index = internal_info.index;

            Ok(Export {
                export,
                internal_derivation_index,
                external_derivation_index,
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use bdk::wallet::AddressIndex;

        const DEPOSIT_TESTNET: [&str; 5] = [
            "tb1qaahw5veemvysq5v6sh5mfs9ewtm4fq90gc308k",
            "tb1qjvjwvxdjgq7rdnje6ytt7edk38es4u3fxad80w",
            "tb1qthqe7rudmw29cufwzhx9q6ewukpyv89xg6sy0x",
            "tb1qax7lx4pp6kw9zptjp845s6g8dewmcx2sdp3mgz",
            "tb1qjjdn6qsq9mu65rmzhrmesuga5hpp0ug0zswrkw",
        ];

        #[test]
        fn testnet4_migrates_using_legacy_testnet_format() {
            assert_eq!(
                crate::wallet::legacy_wallet_network(bdk_wallet::bitcoin::Network::Testnet4)
                    .unwrap(),
                bdk::bitcoin::Network::Testnet
            );
        }

        #[test]
        fn account_xpub_is_extracted_from_a_descriptor() {
            let descriptor = "wpkh([ea8fea5b/84'/1'/2147483643']tpubDDjm783wfQh3p5QW5E7Dy9BWJU3c3RYmiBFZYafp8doVAzKeaie6DUAL1p4bzTF8r8gmTzmMiNmv5L1fsEKJrZupZxAztskqyMgmEqVi4FU/0/*)#gfe7pp57";
            assert_eq!(
                crate::wallet::account_xpub(descriptor).as_deref(),
                Some("tpubDDjm783wfQh3p5QW5E7Dy9BWJU3c3RYmiBFZYafp8doVAzKeaie6DUAL1p4bzTF8r8gmTzmMiNmv5L1fsEKJrZupZxAztskqyMgmEqVi4FU")
            );
            assert_eq!(crate::wallet::account_xpub("sh(wpkh(...))"), None);
        }

        #[test]
        fn v5_deposit_addresses_are_stable_at_known_indices() {
            use bdk_wallet::bitcoin::bip32::Xpriv;
            use bdk_wallet::bitcoin::Network as V5Network;
            use bdk_wallet::template::DescriptorTemplate as _;
            use bdk_wallet::KeychainKind as V5Keychain;

            let xprv = Xpriv::new_master(V5Network::Testnet4, &[7u8; 64]).unwrap();
            let external = crate::wallet::SamouraiBip84(
                xprv,
                V5Keychain::External,
                crate::wallet::WalletAccount::Deposit,
            )
            .build(V5Network::Testnet4)
            .unwrap();
            let internal = crate::wallet::SamouraiBip84(
                xprv,
                V5Keychain::Internal,
                crate::wallet::WalletAccount::Deposit,
            )
            .build(V5Network::Testnet4)
            .unwrap();
            let wallet = bdk_wallet::Wallet::create(external, internal)
                .network(V5Network::Testnet4)
                .create_wallet_no_persist()
                .unwrap();

            let addresses: Vec<String> = [0u32, 11, 129]
                .iter()
                .map(|index| {
                    wallet
                        .peek_address(V5Keychain::External, *index)
                        .address
                        .to_string()
                })
                .collect();
            // Pins the derivation that funded swap cc58b0e8: the swap deposit
            // account's external index 11. A change in account, derivation
            // path, or network would silently move every user deposit.
            assert_eq!(
                addresses,
                vec![
                    "tb1qh0y3qla9eqxffh34yusgv0tvqur8sgcc5gnxnz".to_string(),
                    "tb1qjzmlyu840s0s34rmuqtpz080yrcvf7zfsntrat".to_string(),
                    "tb1qa4al895awfdtk4369pyh3mjvcy5rfg95jspuf4".to_string(),
                ]
            );
        }

        #[test]
        fn legacy_template_matches_swap_deposit_addresses() {
            let xprv = ExtendedPrivKey::new_master(Network::Testnet, &[0u8; 64]).unwrap();
            let wallet = bdk::Wallet::new(
                SamouraiBip84Legacy(xprv, KeychainKind::External, WalletAccount::Deposit),
                Some(SamouraiBip84Legacy(
                    xprv,
                    KeychainKind::Internal,
                    WalletAccount::Deposit,
                )),
                Network::Testnet,
                bdk::database::MemoryDatabase::new(),
            )
            .unwrap();

            let actual: Vec<String> = (0..3)
                .map(|_| {
                    wallet
                        .get_address(AddressIndex::New)
                        .unwrap()
                        .address
                        .to_string()
                })
                .chain((0..2).map(|_| {
                    wallet
                        .get_internal_address(AddressIndex::New)
                        .unwrap()
                        .address
                        .to_string()
                }))
                .collect();
            assert_eq!(actual, DEPOSIT_TESTNET);
        }

        #[tokio::test]
        async fn old_wallet_export_targets_swap_deposit_account() {
            let dir = tempfile::tempdir().unwrap();
            let xprv = ExtendedPrivKey::new_master(Network::Testnet, &[0u8; 64]).unwrap();
            let old = OldWallet::new(dir.path(), xprv, Network::Testnet, WalletAccount::Deposit)
                .await
                .unwrap();
            let export = old.export("test").await.unwrap();
            let json = export.export.to_string();

            assert!(
                json.contains("2147483643"),
                "export must target SWAPS_DEPOSIT: {json}"
            );
            assert!(
                json.contains("84'/1'"),
                "export must be testnet BIP84: {json}"
            );
        }
    }
}

/// Trait for converting a type into an Arc<Mutex<T>>.
// We use this a ton in this file so this is a convenience trait.
trait IntoArcMutex<T> {
    fn into_arc_mutex_async(self) -> Arc<TokioMutex<T>>;
    fn into_arc_mutex_sync(self) -> Arc<SyncMutex<T>>;
}

impl<T> IntoArcMutex<T> for T {
    fn into_arc_mutex_async(self) -> Arc<TokioMutex<T>> {
        Arc::new(TokioMutex::new(self))
    }

    fn into_arc_mutex_sync(self) -> Arc<SyncMutex<T>> {
        Arc::new(SyncMutex::new(self))
    }
}

/// For tests
#[derive(Clone)]
pub struct StaticFeeRate {
    fee_rate: FeeRate,
    min_relay_fee: bitcoin::Amount,
}

impl StaticFeeRate {
    pub fn new(fee_rate: FeeRate, min_relay_fee: bitcoin::Amount) -> Self {
        Self {
            fee_rate,
            min_relay_fee,
        }
    }
}

impl EstimateFeeRate for StaticFeeRate {
    async fn estimate_feerate(&self, _target_block: u32) -> Result<FeeRate> {
        Ok(self.fee_rate)
    }

    async fn min_relay_fee(&self) -> Result<FeeRate> {
        Ok(FeeRate::from_sat_per_vb(self.min_relay_fee.to_sat()).unwrap())
    }
}

#[derive(Debug)]
pub struct TestWalletBuilder {
    utxo_amount: u64,
    sats_per_vb: u64,
    min_relay_sats_per_vb: u64,
    key: bitcoin::bip32::Xpriv,
    num_utxos: u8,
    account: WalletAccount,
    dojo: Option<Arc<DojoFeeEstimator>>,
    require_dojo: bool,
}

impl TestWalletBuilder {
    /// Creates a new, funded wallet with sane default fees.
    ///
    /// Unless you are testing things related to fees, this is likely what you
    /// want.
    pub fn new(amount: u64) -> Self {
        TestWalletBuilder {
            utxo_amount: amount,
            sats_per_vb: 1,
            min_relay_sats_per_vb: 1,
            key: "tprv8ZgxMBicQKsPeZRHk4rTG6orPS2CRNFX3njhUXx5vj9qGog5ZMH4uGReDWN5kCkY3jmWEtWause41CDvBRXD1shKknAMKxT99o9qUTRVC6m".parse().unwrap(),
            num_utxos: 1,
            account: WalletAccount::Deposit,
            dojo: None,
            require_dojo: false,
        }
    }

    pub fn with_zero_fees(self) -> Self {
        Self {
            sats_per_vb: 0,
            min_relay_sats_per_vb: 0,
            ..self
        }
    }

    pub fn with_fees(self, sats_per_vb: u64, min_relay_sats_per_vb: u64) -> Self {
        Self {
            sats_per_vb,
            min_relay_sats_per_vb,
            ..self
        }
    }

    pub fn with_key(self, key: bitcoin::bip32::Xpriv) -> Self {
        Self { key, ..self }
    }

    pub fn with_num_utxos(self, number: u8) -> Self {
        Self {
            num_utxos: number,
            ..self
        }
    }

    pub fn with_account(self, account: WalletAccount) -> Self {
        Self { account, ..self }
    }

    pub fn with_dojo(self, dojo: Arc<DojoFeeEstimator>, require_dojo: bool) -> Self {
        Self {
            dojo: Some(dojo),
            require_dojo,
            ..self
        }
    }

    pub async fn build(self) -> Wallet<Connection, StaticFeeRate> {
        let client = StaticFeeRate::new(
            FeeRate::from_sat_per_vb(self.sats_per_vb).unwrap(),
            bitcoin::Amount::from_sat(self.min_relay_sats_per_vb),
        );
        self.build_with_fee_strategy(client).await
    }

    pub async fn build_with_fee_strategy<C>(self, client: C) -> Wallet<Connection, C>
    where
        C: EstimateFeeRate + Send + Sync + Clone + 'static,
    {
        use bdk_wallet::chain::BlockId;
        use bdk_wallet::test_utils::{insert_checkpoint, receive_output_in_latest_block};

        let bdk_network = bitcoin::Network::Regtest;

        let external_descriptor = SamouraiBip84(self.key, KeychainKind::External, self.account)
            .build(bdk_network)
            .expect("Failed to build external descriptor for test wallet");
        let internal_descriptor = SamouraiBip84(self.key, KeychainKind::Internal, self.account)
            .build(bdk_network)
            .expect("Failed to build internal descriptor for test wallet");

        let mut persister = bdk_wallet::rusqlite::Connection::open_in_memory()
            .expect("Failed to open in-memory DB for test wallet");

        let bdk_core_wallet = bdk_wallet::Wallet::create(external_descriptor, internal_descriptor)
            .network(bdk_network)
            .create_wallet(&mut persister)
            .expect("Failed to create bdk_wallet::Wallet for test");

        let cached_electrum_fee_estimator = Arc::new(CachedFeeEstimator::new(client.clone()));

        let wallet = Wallet {
            wallet: bdk_core_wallet.into_arc_mutex_async(),
            electrum_client: Arc::new(client),
            cached_electrum_fee_estimator,
            cached_mempool_fee_estimator: Arc::new(None), // We don't use mempool client in tests
            persister: persister.into_arc_mutex_async(),
            tauri_handle: None,
            network: Network::Regtest,
            finality_confirmations: 1,
            target_block: 1,
            xprivkey: self.key,
            dojo: Arc::new(TokioRwLock::new(self.dojo)),
            require_dojo: Arc::new(AtomicBool::new(self.require_dojo)),
            sync_window: Arc::new(SyncWindow::default()),
        };

        let mut locked_wallet = wallet.wallet.try_lock().unwrap();

        // Create a block
        insert_checkpoint(
            &mut locked_wallet,
            BlockId {
                height: 42,
                hash: <bitcoin::blockdata::block::BlockHash as bitcoin::hashes::Hash>::all_zeros(),
            },
        );

        // Fund the wallet with fake utxos
        for _ in 0..self.num_utxos {
            receive_output_in_latest_block(&mut locked_wallet, Amount::from_sat(self.utxo_amount));
        }

        // Create another block to confirm the utxos
        insert_checkpoint(
            &mut locked_wallet,
            BlockId {
                height: 43,
                hash: <bitcoin::blockdata::block::BlockHash as bitcoin::hashes::Hash>::all_zeros(),
            },
        );

        drop(locked_wallet);

        wallet
    }
}

/// Implement BitcoinWallet trait for a stub wallet and panic when the function is not implemented
#[async_trait::async_trait]
#[allow(unused)]
impl BitcoinWallet for Wallet<Connection, StaticFeeRate> {
    async fn ensure_broadcasted(
        &self,
        tx: bitcoin::Transaction,
        kind: &str,
    ) -> Result<(Txid, Subscription)> {
        unimplemented!("stub method called erroneously")
    }

    async fn balance(&self) -> Result<Amount> {
        unimplemented!("stub method called erroneously")
    }

    async fn balance_info(&self) -> Result<Balance> {
        unimplemented!("stub method called erroneously")
    }

    async fn new_address(&self) -> Result<Address> {
        Wallet::new_address(self).await
    }

    async fn send_to_address(
        &self,
        address: Address,
        amount: Amount,
        spending_fee: Amount,
        change_override: Option<Address>,
        max_inputs: Option<usize>,
    ) -> Result<Psbt> {
        Wallet::send_to_address(
            self,
            address,
            amount,
            spending_fee,
            change_override,
            max_inputs,
        )
        .await
    }

    async fn send_to_address_dynamic_fee(
        &self,
        address: Address,
        amount: Amount,
        change_override: Option<Address>,
        max_inputs: Option<usize>,
    ) -> Result<Psbt> {
        unimplemented!("stub method called erroneously")
    }

    async fn sweep_balance_to_address_dynamic_fee(&self, address: Address) -> Result<Psbt> {
        unimplemented!("stub method called erroneously")
    }

    async fn sign_and_finalize(&self, psbt: Psbt) -> Result<bitcoin::Transaction> {
        Wallet::sign_and_finalize(self, psbt).await
    }

    fn prove_utxo_ownership(
        &self,
        psbt: &Psbt,
        challenge: utxo_ownership_proof::Challenge,
    ) -> Result<Vec<utxo_ownership_proof::OwnershipProof>> {
        Wallet::prove_utxo_ownership(self, psbt, challenge)
    }

    async fn sync(&self) -> Result<()> {
        unimplemented!("stub method called erroneously")
    }

    async fn health_check(&self) -> Result<()> {
        unimplemented!("stub method called erroneously")
    }

    async fn subscribe_to(&self, tx: Box<dyn Watchable>) -> Subscription {
        unimplemented!("stub method called erroneously")
    }

    async fn status_of_script(&self, tx: &dyn Watchable) -> Result<ScriptStatus> {
        unimplemented!("stub method called erroneously")
    }

    async fn get_raw_transaction(
        &self,
        txid: Txid,
    ) -> Result<Option<std::sync::Arc<bitcoin::Transaction>>> {
        unimplemented!("stub method called erroneously")
    }

    async fn max_giveable(
        &self,
        locking_script_size: usize,
        max_inputs: Option<usize>,
    ) -> Result<(Amount, Amount)> {
        unimplemented!("stub method called erroneously")
    }

    async fn estimate_fee(
        &self,
        weight: Weight,
        transfer_amount: Option<Amount>,
    ) -> Result<Amount> {
        Wallet::estimate_fee(self, weight, transfer_amount).await
    }

    async fn swap_fee_rate(&self) -> Result<FeeRate> {
        Wallet::swap_fee_rate(self).await
    }

    fn network(&self) -> Network {
        self.network
    }

    fn finality_confirmations(&self) -> u32 {
        unimplemented!("stub method called erroneously")
    }

    async fn wallet_export(&self, role: &str) -> Result<FullyNodedExport> {
        unimplemented!("stub method called erroneously")
    }
}

#[cfg(test)]
mod input_limit_tests {
    use super::*;
    const INPUT_LIMIT: usize = 25;

    #[tokio::test]
    async fn fragmented_wallet_estimate_and_lock_construction_use_the_same_cap() -> Result<()> {
        let wallet = TestWalletBuilder::new(100_000)
            .with_num_utxos(26)
            .build()
            .await;
        let recipient = wallet.new_address().await?;
        let (amount, fee) = wallet
            .max_giveable(recipient.script_pubkey().len(), Some(INPUT_LIMIT))
            .await?;
        assert_eq!(amount + fee, Amount::from_sat(2_500_000));
        let psbt = wallet
            .send_to_address(recipient.clone(), amount, fee, None, Some(INPUT_LIMIT))
            .await?;
        assert_eq!(psbt.inputs.len(), INPUT_LIMIT);
        assert_eq!(psbt.fee()?, fee);
        assert_eq!(psbt.unsigned_tx.output.len(), 1);
        assert_eq!(psbt.unsigned_tx.output[0].value, amount);
        wallet.sign_and_finalize(psbt).await?;
        let error = wallet
            .send_to_address(
                recipient.clone(),
                Amount::from_sat(2_500_000),
                fee,
                None,
                Some(INPUT_LIMIT),
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Failed to select transaction inputs")
        );
        assert!(
            wallet
                .send_to_address_dynamic_fee(
                    recipient,
                    Amount::from_sat(2_500_000),
                    None,
                    Some(INPUT_LIMIT)
                )
                .await
                .is_err()
        );
        Ok(())
    }

    #[tokio::test]
    async fn unrestricted_withdrawal_can_spend_every_fragment() -> Result<()> {
        let wallet = TestWalletBuilder::new(100_000)
            .with_num_utxos(30)
            .build()
            .await;
        let recipient = wallet.new_address().await?;
        let (amount, fee) = wallet
            .max_giveable(recipient.script_pubkey().len(), None)
            .await?;
        assert_eq!(amount + fee, Amount::from_sat(3_000_000));
        let psbt = wallet
            .sweep_balance_to_address_dynamic_fee(recipient.clone())
            .await?;
        assert_eq!(psbt.inputs.len(), 30);
        assert_eq!(psbt.fee()?, fee);
        assert_eq!(psbt.unsigned_tx.output[0].value, amount);
        wallet.sign_and_finalize(psbt).await?;
        let psbt = wallet
            .send_to_address_dynamic_fee(recipient, Amount::from_sat(2_800_000), None, None)
            .await?;
        assert!(psbt.inputs.len() > INPUT_LIMIT);
        Ok(())
    }

    #[tokio::test]
    async fn limited_selection_uses_largest_spendable_outputs() -> Result<()> {
        let wallet = TestWalletBuilder::new(100_000)
            .with_num_utxos(30)
            .build()
            .await;
        let largest = wallet.insert_fake_utxo(Amount::from_sat(500_000)).await;
        let second = wallet.insert_fake_utxo(Amount::from_sat(200_000)).await;
        let immature = {
            let mut inner = wallet.wallet.lock().await;
            let tx = Transaction {
                version: bitcoin::transaction::Version::TWO,
                lock_time: bitcoin::absolute::LockTime::ZERO,
                input: vec![bitcoin::TxIn::default()], // null prevout marks a coinbase
                output: vec![bitcoin::TxOut {
                    value: Amount::ONE_BTC,
                    script_pubkey: inner
                        .next_unused_address(KeychainKind::External)
                        .script_pubkey(),
                }],
            };
            let txid = tx.compute_txid();
            let anchor = bdk_chain::ConfirmationBlockTime {
                block_id: inner.latest_checkpoint().block_id(),
                confirmation_time: 0,
            };
            let mut update = bdk_wallet::Update::default();
            update.tx_update.txs.push(Arc::new(tx));
            update.tx_update.anchors.insert((anchor, txid));
            inner.apply_update(update)?;
            assert_eq!(inner.balance().immature, Amount::ONE_BTC);
            bitcoin::OutPoint { txid, vout: 0 }
        };
        let recipient = wallet.new_address().await?;
        let (amount, fee) = wallet
            .max_giveable(recipient.script_pubkey().len(), Some(3))
            .await?;
        assert_eq!(amount + fee, Amount::from_sat(800_000));
        let psbt = wallet
            .send_to_address(recipient.clone(), amount, fee, None, Some(3))
            .await?;
        let selected: Vec<_> = psbt
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output)
            .collect();
        assert!(selected.contains(&largest) && selected.contains(&second));
        assert!(!selected.contains(&immature));
        assert_eq!(selected.len(), 3);
        let dynamic = wallet
            .send_to_address_dynamic_fee(recipient, Amount::from_sat(750_000), None, Some(3))
            .await?;
        assert_eq!(dynamic.inputs.len(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn input_limit_must_allow_at_least_one_input() {
        let wallet = TestWalletBuilder::new(100_000).build().await;
        assert!(wallet.max_giveable(22, Some(0)).await.is_err());
        assert!(wallet.max_giveable(22, Some(1)).await.is_ok());
    }

    #[tokio::test]
    async fn unconfirmed_v3_outputs_are_not_spent() -> Result<()> {
        use bdk_wallet::chain::ConfirmationBlockTime;
        use bdk_wallet::test_utils::{insert_anchor, insert_tx};

        let wallet = TestWalletBuilder::new(100_000).build().await;
        let receive = |version, sats| {
            let wallet = &wallet;
            async move {
                let mut bdk = wallet.wallet.lock().await;
                let script_pubkey = bdk
                    .next_unused_address(KeychainKind::External)
                    .script_pubkey();
                let tx = Transaction {
                    version: bitcoin::transaction::Version(version),
                    lock_time: bitcoin::absolute::LockTime::ZERO,
                    input: vec![],
                    output: vec![bitcoin::TxOut {
                        value: Amount::from_sat(sats),
                        script_pubkey,
                    }],
                };
                let txid = tx.compute_txid();
                insert_tx(&mut bdk, tx);
                bitcoin::OutPoint::new(txid, 0)
            }
        };
        let v2_output = receive(2, 30_000).await;
        let v3_output = receive(3, 50_000).await;

        // Drains everything spendable and returns the spent outpoints.
        let drain = || async {
            let recipient = wallet.new_address().await?;
            let (amount, fee) = wallet
                .max_giveable(recipient.script_pubkey().len(), None)
                .await?;
            let psbt = wallet
                .send_to_address(recipient, amount, fee, None, None)
                .await?;
            let spent: Vec<_> = psbt
                .unsigned_tx
                .input
                .iter()
                .map(|input| input.previous_output)
                .collect();
            Ok::<_, anyhow::Error>((amount + fee, spent))
        };

        let (total, spent) = drain().await?;
        assert_eq!(total, Amount::from_sat(130_000));
        assert!(spent.contains(&v2_output));
        assert!(!spent.contains(&v3_output));

        {
            let mut bdk = wallet.wallet.lock().await;
            let block_id = bdk.latest_checkpoint().block_id();
            insert_anchor(
                &mut bdk,
                v3_output.txid,
                ConfirmationBlockTime {
                    block_id,
                    confirmation_time: 0,
                },
            );
        }

        let (total, spent) = drain().await?;
        assert_eq!(total, Amount::from_sat(180_000));
        assert!(spent.contains(&v3_output));
        Ok(())
    }

    #[tokio::test]
    async fn fees_cover_the_signed_transaction() -> Result<()> {
        let sats_per_vb = 10;
        for inputs in [1, INPUT_LIMIT] {
            let wallet = TestWalletBuilder::new(100_000)
                .with_fees(sats_per_vb, 1)
                .with_num_utxos(u8::try_from(inputs)?)
                .build()
                .await;
            let recipient = wallet.new_address().await?;
            let (amount, fee) = wallet
                .max_giveable(recipient.script_pubkey().len(), Some(INPUT_LIMIT))
                .await?;
            let max_giveable = wallet
                .send_to_address(recipient.clone(), amount, fee, None, Some(INPUT_LIMIT))
                .await?;
            let dynamic_fee = wallet
                .send_to_address_dynamic_fee(recipient, amount / 2, None, Some(INPUT_LIMIT))
                .await?;

            for psbt in [max_giveable, dynamic_fee] {
                let fee = psbt.fee()?;
                let signed = wallet.sign_and_finalize(psbt).await?;
                assert!(
                    fee.to_sat() >= sats_per_vb * signed.vsize() as u64,
                    "{fee} for {} vB with {inputs} inputs",
                    signed.vsize()
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod samourai_compat_tests {
    use super::*;

    const TESTNET_FIXTURES: [(&str, [&str; 5]); 3] = [
        (
            "deposit",
            [
                "tb1qaahw5veemvysq5v6sh5mfs9ewtm4fq90gc308k",
                "tb1qjvjwvxdjgq7rdnje6ytt7edk38es4u3fxad80w",
                "tb1qthqe7rudmw29cufwzhx9q6ewukpyv89xg6sy0x",
                "tb1qax7lx4pp6kw9zptjp845s6g8dewmcx2sdp3mgz",
                "tb1qjjdn6qsq9mu65rmzhrmesuga5hpp0ug0zswrkw",
            ],
        ),
        (
            "refund",
            [
                "tb1qk7ej4wdn24wya35leukqz6h7ne8sd6ertgzucw",
                "tb1qz7uxxsu4req0ejvwmy672ymage87y5zmrulgld",
                "tb1qrhyuf4dqtute3vsa6tew0dtys2l2kv4n8r8luq",
                "tb1qx7y0qu800ftd3cjxyjwf4p95ttw4zh85nx2xq9",
                "tb1qf0tdx3e5zjysl3wumraa6s7xfcsu46r88d3ly5",
            ],
        ),
        (
            "asb",
            [
                "tb1qn545nt5fj5xsx6smdyds569ke0gh4a56j90ds4",
                "tb1qyasfnk6fte9v46zctnaa4dh8jtkslettr7zg40",
                "tb1q2v9hmf7sj6kvg5wtj6cz63usd8t230feuj683r",
                "tb1q3fqvj0txhhst8v4c75064sqz2drg6rlxxk9lxs",
                "tb1qyxnxgv2wy7aw020mg3wlmk92dtmqcr4s9jhnsz",
            ],
        ),
    ];

    const MAINNET_FIXTURES: [(&str, [&str; 5]); 3] = [
        (
            "deposit",
            [
                "bc1qd0v8yanfxdcp6aex0wy7fk27cvra7eyn08awcr",
                "bc1q89km3erx2e6nj98ur7aem07ejsqcwm4hxn2exw",
                "bc1qsk2l25h9ja544xxhmsq88hlwyqqszc47rklt2k",
                "bc1q9pevw0hzcym2wx999cfepxmddvct5zgywn76wg",
                "bc1q04p5uuf355l49ag8ts2la9dc9rar8aj9v7my5h",
            ],
        ),
        (
            "refund",
            [
                "bc1qvs5ra7r6n9drch0lxxh2yswh48uh8s87n204y5",
                "bc1q224afdju4e259e5j7aknqc4cypjxq85rwyqxxj",
                "bc1qs3y9a5nd0a9j2h7g5ez6drd2287y85jvtamq0k",
                "bc1qgq2ae48qmplvq2x4eygckcdg6682xeepjzc454",
                "bc1q83uznxjqa6yvnyd9r2pn4xpmcacd7dhcyy38l5",
            ],
        ),
        (
            "asb",
            [
                "bc1q7l2rr9ktnfpcwetemcj48agm5jf3l9yyh883vn",
                "bc1qd0wz8cmqkcsqat5rss042ltkvu5j33pvlw5nwg",
                "bc1qyzcc27a05g4d7js834r5ml0x90ng8vcjxdjp4c",
                "bc1qd2j2akj7h6ypy83wmk4fzpq70ujm20kuegl2zg",
                "bc1qf84vw6t03un4kvm3a43c5dxgcxjl9pn9308hjc",
            ],
        ),
    ];

    fn account(label: &str) -> WalletAccount {
        match label {
            "deposit" => WalletAccount::Deposit,
            "refund" => WalletAccount::Refund,
            "asb" => WalletAccount::Asb,
            _ => panic!("unknown account {label}"),
        }
    }

    fn assert_addresses(network: Network, fixtures: &[(&str, [&str; 5]); 3]) {
        let xpriv = Xpriv::new_master(network, &[0u8; 64]).unwrap();
        for (label, expected) in fixtures {
            let mut wallet = bdk_wallet::Wallet::create(
                SamouraiBip84(xpriv, KeychainKind::External, account(label)),
                SamouraiBip84(xpriv, KeychainKind::Internal, account(label)),
            )
            .network(network)
            .create_wallet_no_persist()
            .unwrap();

            let receive: Vec<String> = (0..3)
                .map(|_| {
                    wallet
                        .reveal_next_address(KeychainKind::External)
                        .address
                        .to_string()
                })
                .collect();
            let change: Vec<String> = (0..2)
                .map(|_| {
                    wallet
                        .reveal_next_address(KeychainKind::Internal)
                        .address
                        .to_string()
                })
                .collect();
            let actual: Vec<String> = receive.into_iter().chain(change).collect();
            assert_eq!(actual, expected, "{label} on {network}");
        }
    }

    #[test]
    fn account_numbers_are_stable() {
        assert_eq!(WalletAccount::Deposit.account_number(), 2_147_483_643);
        assert_eq!(WalletAccount::Refund.account_number(), 2_147_483_642);
        assert_eq!(WalletAccount::Asb.account_number(), 2_147_483_641);
    }

    #[test]
    fn testnet_addresses_match_current_outpost_wallet() {
        assert_addresses(Network::Testnet, &TESTNET_FIXTURES);
    }

    #[test]
    fn mainnet_addresses_match_current_outpost_wallet() {
        assert_addresses(Network::Bitcoin, &MAINNET_FIXTURES);
    }
}

#[cfg(test)]
mod dojo_wallet_tests {
    use super::*;
    use crate::DojoFeeEstimator;
    use bitcoin::hashes::Hash;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use url::Url;

    async fn spawn_dojo(
        fees: &'static str,
        fail_first_push: bool,
    ) -> (Url, tokio::task::JoinHandle<()>, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let pushes = Arc::new(AtomicUsize::new(0));
        let pushes_for_server = Arc::clone(&pushes);

        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 16 * 1024];
                let read = socket.read(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf[..read]);

                let (status, body) = if head.contains("POST /auth/login") {
                    (
                        "200 OK",
                        "{\"authorizations\":{\"access_token\":\"tok-123\"}}".to_string(),
                    )
                } else if head.contains("GET /fees/estimator") {
                    ("200 OK", fees.to_string())
                } else if head.contains("GET /tx/") {
                    (
                        "200 OK",
                        "{\"txid\":\"abc\",\"confirmations\":1,\"block\":{\"height\":100}}"
                            .to_string(),
                    )
                } else if head.contains("POST /pushtx/") {
                    let push = pushes_for_server.fetch_add(1, Ordering::SeqCst) + 1;
                    if fail_first_push && push == 1 {
                        (
                            "500 Internal Server Error",
                            "{\"error\":\"temporary\"}".to_string(),
                        )
                    } else {
                        (
                            "200 OK",
                            "{\"status\":\"ok\",\"data\":\"txid\"}".to_string(),
                        )
                    }
                } else {
                    ("404 Not Found", "{}".to_string())
                };

                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        (
            Url::parse(&format!("http://{addr}")).unwrap(),
            server,
            pushes,
        )
    }

    fn estimator(url: Url) -> Arc<DojoFeeEstimator> {
        Arc::new(DojoFeeEstimator::new(url, "test-api-key".to_string(), 0).unwrap())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dojo_rate_is_preferred_over_electrum() {
        let (url, server, _) = spawn_dojo("{\"0.999\":21}", false).await;
        let wallet = TestWalletBuilder::new(100_000)
            .with_fees(5, 1)
            .with_dojo(estimator(url), false)
            .build()
            .await;

        assert_eq!(
            wallet.swap_fee_rate().await.unwrap().to_sat_per_vb_ceil(),
            21
        );
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn target_specific_fee_estimates_use_the_requested_confidence() {
        let (url, server, _) = spawn_dojo("{\"0.5\":3,\"0.999\":40}", false).await;
        let wallet = TestWalletBuilder::new(100_000)
            .with_fees(5, 1)
            .with_dojo(estimator(url), false)
            .build()
            .await;
        let amount = Amount::from_sat(100_000_000);

        assert_eq!(
            wallet
                .estimate_fee_vsize_at(1_000, amount, 1)
                .await
                .unwrap(),
            Amount::from_sat(40_000)
        );
        assert_eq!(
            wallet
                .estimate_fee_vsize_at(1_000, amount, 6)
                .await
                .unwrap(),
            Amount::from_sat(3_000)
        );
        assert_eq!(
            wallet
                .estimate_fee_vsize_at_with_rate_multiplier(1_000, amount, 1, 1.25)
                .await
                .unwrap(),
            Amount::from_sat(50_000)
        );
        assert_eq!(
            wallet.estimate_fee_at(4_000, amount, 6).await.unwrap(),
            Amount::from_sat(3_000)
        );

        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn required_dojo_failure_does_not_fall_back_to_electrum() {
        let dead = Arc::new(
            DojoFeeEstimator::new(
                Url::parse("http://127.0.0.1:1").unwrap(),
                "unused".to_string(),
                0,
            )
            .unwrap()
            .with_retry_budget(Duration::ZERO),
        );
        let wallet = TestWalletBuilder::new(100_000)
            .with_fees(5, 1)
            .with_dojo(dead.clone(), true)
            .build()
            .await;

        let error = wallet.swap_fee_rate().await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Dojo fee estimation is required"),
            "unexpected error: {error:#}"
        );

        let fallback = TestWalletBuilder::new(100_000)
            .with_fees(5, 1)
            .with_dojo(dead, false)
            .build()
            .await;
        assert_eq!(
            fallback.swap_fee_rate().await.unwrap().to_sat_per_vb_ceil(),
            5,
            "a non-required Dojo failure must fall back to Electrum"
        );
    }

    #[tokio::test]
    async fn dojo_broadcast_retries_once_then_succeeds() {
        let attempts = std::cell::Cell::new(0);
        let published = broadcast_via_dojo(Txid::all_zeros(), "lock", 2, Duration::ZERO, || {
            let attempt = attempts.get() + 1;
            attempts.set(attempt);
            async move {
                if attempt == 1 {
                    Err(anyhow!("temporary Dojo timeout"))
                } else {
                    Ok(())
                }
            }
        })
        .await;

        assert!(published);
        assert_eq!(attempts.get(), 2);
    }

    #[tokio::test]
    async fn dojo_broadcast_stops_after_configured_attempts() {
        let attempts = std::cell::Cell::new(0);
        let published = broadcast_via_dojo(Txid::all_zeros(), "lock", 2, Duration::ZERO, || {
            attempts.set(attempts.get() + 1);
            async { Err(anyhow!("Dojo unavailable")) }
        })
        .await;

        assert!(!published);
        assert_eq!(attempts.get(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dojo_broadcast_retries_real_http_failure_then_succeeds() {
        let (url, server, pushes) = spawn_dojo("{}", true).await;
        let estimator = estimator(url);

        let published = broadcast_via_dojo(
            Txid::all_zeros(),
            "lock",
            2,
            Duration::from_millis(10),
            || estimator.broadcast("deadbeef"),
        )
        .await;

        assert!(published);
        assert_eq!(pushes.load(Ordering::SeqCst), 2);
        server.abort();
    }

    /// A checker that answers `proves` and records the (txid, height) it was asked about.
    fn recording_checker(
        proves: bool,
    ) -> (
        crate::primitives::InclusionChecker,
        Arc<std::sync::Mutex<Vec<(Txid, u32)>>>,
    ) {
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = Arc::clone(&asked);
        let checker = crate::primitives::InclusionChecker(Arc::new(move |txid, height| {
            record.lock().unwrap().push((txid, height));
            Box::pin(async move { Ok(proves) })
        }));
        (checker, asked)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn subscription_accepts_dojo_confirmation_when_electrum_lags() {
        let (url, server, _) = spawn_dojo("{}", false).await;
        let (_sender, receiver) = watch::channel(ScriptStatus::Unseen);
        let txid = Txid::from_byte_array([1u8; 32]);
        let (checker, asked) = recording_checker(true);
        let subscription = Subscription::new(receiver, 1, txid)
            .with_dojo(Some(estimator(url)))
            .with_inclusion_checker(Some(checker));

        tokio::time::timeout(Duration::from_secs(5), subscription.wait_until_final())
            .await
            .expect("a proven Dojo confirmation must break the stalled Electrum feed")
            .unwrap();

        // The proof was asked for at the block the Dojo named
        assert_eq!(asked.lock().unwrap().as_slice(), &[(txid, 100)]);
        server.abort();
    }

    /// Audit H-4: the Dojo's word alone no longer ends a wait.
    #[tokio::test(flavor = "multi_thread")]
    async fn dojo_confirmation_without_a_proof_check_does_not_end_the_wait() {
        let (url, server, _) = spawn_dojo("{}", false).await;
        let (_sender, receiver) = watch::channel(ScriptStatus::Unseen);
        let subscription = Subscription::new(receiver, 1, Txid::from_byte_array([1u8; 32]))
            .with_dojo(Some(estimator(url)));

        let waited =
            tokio::time::timeout(Duration::from_secs(2), subscription.wait_until_final()).await;

        assert!(waited.is_err(), "an unproven Dojo confirmation must not end the wait");
        server.abort();
    }

    /// Audit H-4: a Dojo claim that no Electrum server can prove keeps the wait going.
    #[tokio::test(flavor = "multi_thread")]
    async fn dojo_confirmation_that_fails_the_merkle_proof_does_not_end_the_wait() {
        let (url, server, _) = spawn_dojo("{}", false).await;
        let (_sender, receiver) = watch::channel(ScriptStatus::Unseen);
        let (checker, asked) = recording_checker(false);
        let subscription = Subscription::new(receiver, 1, Txid::from_byte_array([1u8; 32]))
            .with_dojo(Some(estimator(url)))
            .with_inclusion_checker(Some(checker));

        let waited =
            tokio::time::timeout(Duration::from_secs(2), subscription.wait_until_final()).await;

        assert!(waited.is_err(), "a refuted Dojo confirmation must not end the wait");
        assert!(!asked.lock().unwrap().is_empty(), "the proof must have been tried");
        server.abort();
    }

    #[tokio::test]
    async fn dojo_claim_rules() {
        use crate::DojoTxView;
        use crate::primitives::dojo_claim_is_proven;
        let txid = Txid::from_byte_array([2u8; 32]);
        let (yes, _) = recording_checker(true);
        let (no, _) = recording_checker(false);
        let mined = DojoTxView { confirmations: 1, block_height: Some(100) };

        assert!(dojo_claim_is_proven(txid, mined, 1, Some(&yes)).await);
        assert!(!dojo_claim_is_proven(txid, mined, 1, Some(&no)).await);
        assert!(!dojo_claim_is_proven(txid, mined, 1, None).await);
        // Too few confirmations: the proof is not even asked for
        let (counting, asked) = recording_checker(true);
        assert!(!dojo_claim_is_proven(txid, mined, 2, Some(&counting)).await);
        assert!(asked.lock().unwrap().is_empty());
        // No block named: nothing to prove against
        let unnamed = DojoTxView { confirmations: 3, block_height: None };
        assert!(!dojo_claim_is_proven(txid, unnamed, 1, Some(&yes)).await);
        // A failing check is not a pass
        let failing = crate::primitives::InclusionChecker(Arc::new(|_, _| {
            Box::pin(async { Err(anyhow!("servers unreachable")) })
        }));
        assert!(!dojo_claim_is_proven(txid, mined, 1, Some(&failing)).await);
    }

    #[tokio::test]
    async fn wallet_spends_are_not_rbf_enabled() -> Result<()> {
        let wallet = TestWalletBuilder::new(100_000).build().await;
        let recipient = wallet.new_address().await?;
        let psbt = wallet
            .send_to_address(
                recipient,
                Amount::from_sat(50_000),
                Amount::from_sat(1_000),
                None,
                None,
            )
            .await?;

        assert!(
            psbt.unsigned_tx
                .input
                .iter()
                .all(|input| !input.sequence.is_rbf()),
            "wallet-built transactions must not opt into RBF"
        );
        Ok(())
    }

    #[test]
    fn fee_policy_caps_are_enforced() {
        let rate = |sats_per_vb| FeeRate::from_sat_per_vb(sats_per_vb).unwrap();
        let min_relay = rate(1);

        let relative = estimate_fee(
            Weight::from_wu(4_000),
            Some(Amount::from_sat(10_000)),
            rate(100),
            min_relay,
        )
        .unwrap();
        assert_eq!(relative, Amount::from_sat(2_000));

        let absolute = estimate_fee(
            Weight::from_wu(8_000),
            Some(Amount::from_sat(100_000_000)),
            rate(100),
            min_relay,
        )
        .unwrap();
        assert_eq!(absolute, MAX_ABSOLUTE_TX_FEE);

        let minimum = estimate_fee(
            Weight::from_wu(400),
            Some(Amount::from_sat(10_000)),
            min_relay,
            min_relay,
        )
        .unwrap();
        assert_eq!(minimum, MIN_ABSOLUTE_TX_FEE);

        assert!(
            estimate_fee(
                Weight::from_wu(400),
                Some(DUST_AMOUNT),
                min_relay,
                min_relay,
            )
            .is_err()
        );
    }

    /// Precedence when the relative cap and the 1000 sat relay floor disagree (matrix F-03):
    /// the 20% cap is applied first and returns even when it is below the floor, so a tiny
    /// transfer can get a fee that nodes will not relay. This matches upstream and fails safe
    /// (the broadcast is rejected); pinned here so a change is deliberate.
    #[test]
    fn the_relative_cap_beats_the_relay_floor_on_a_tiny_transfer() {
        let rate = |sats_per_vb| FeeRate::from_sat_per_vb(sats_per_vb).unwrap();
        let tiny = Amount::from_sat(2_000);

        let fee = estimate_fee(Weight::from_wu(400), Some(tiny), rate(20), rate(1)).unwrap();

        // recommended = 100 vB * 20 = 2000 sat, cap = 20% of 2000 = 400 sat
        assert_eq!(fee, Amount::from_sat(400));
        assert!(fee < MIN_ABSOLUTE_TX_FEE);

        // with the amount unknown there is no cap, so the floor and the estimate apply
        let unknown = estimate_fee(Weight::from_wu(400), None, rate(1), rate(1)).unwrap();
        assert_eq!(unknown, MIN_ABSOLUTE_TX_FEE);
    }
}

#[cfg(test)]
mod broadcast_race_tests {
    use super::*;
    use bitcoin::hashes::Hash;
    use std::cell::RefCell;

    fn tx_key() -> (Txid, ScriptBuf) {
        (Txid::from_byte_array([7; 32]), ScriptBuf::new())
    }

    #[tokio::test]
    async fn known_lock_is_not_rebroadcast() {
        let broadcasts = RefCell::new(0);
        let result = broadcast_if_unknown(
            tx_key().0,
            tx_key().1,
            |_| async { Ok(ScriptStatus::InMempool) },
            || async {
                *broadcasts.borrow_mut() += 1;
                Ok(())
            },
        )
        .await
        .unwrap();

        assert!(!result);
        assert_eq!(*broadcasts.borrow(), 0);
    }

    #[tokio::test]
    async fn definitely_unseen_lock_is_broadcast_once() {
        let broadcasts = RefCell::new(0);
        let result = broadcast_if_unknown(
            tx_key().0,
            tx_key().1,
            |_| async { Ok(ScriptStatus::Unseen) },
            || async {
                *broadcasts.borrow_mut() += 1;
                Ok(())
            },
        )
        .await
        .unwrap();

        assert!(result);
        assert_eq!(*broadcasts.borrow(), 1);
    }

    #[tokio::test]
    async fn unknown_electrum_status_fails_closed_without_broadcast() {
        let broadcasts = RefCell::new(0);
        let result = broadcast_if_unknown(
            tx_key().0,
            tx_key().1,
            |_| async { Ok(ScriptStatus::Retrying) },
            || async {
                *broadcasts.borrow_mut() += 1;
                Ok(())
            },
        )
        .await;

        assert!(result.is_err());
        assert_eq!(*broadcasts.borrow(), 0);
    }

    #[tokio::test]
    async fn timed_out_broadcast_is_success_if_electrum_reports_seen() {
        let statuses =
            RefCell::new(vec![ScriptStatus::Unseen, ScriptStatus::InMempool].into_iter());
        let result = broadcast_if_unknown(
            tx_key().0,
            tx_key().1,
            |_| {
                let status = statuses.borrow_mut().next().unwrap();
                async move { Ok(status) }
            },
            || async { Err(anyhow!("broadcast response timed out")) },
        )
        .await
        .unwrap();

        assert!(!result);
        assert!(statuses.borrow_mut().next().is_none());
    }

    #[tokio::test]
    async fn broadcast_error_with_unseen_lock_is_returned_for_same_tx_retry() {
        let statuses = RefCell::new(vec![ScriptStatus::Unseen, ScriptStatus::Unseen].into_iter());
        let result = broadcast_if_unknown(
            tx_key().0,
            tx_key().1,
            |_| {
                let status = statuses.borrow_mut().next().unwrap();
                async move { Ok(status) }
            },
            || async { Err(anyhow!("temporary Electrum error")) },
        )
        .await;

        assert!(result.is_err());
        assert!(statuses.borrow_mut().next().is_none());
    }
}

/// A fixed root key for callers that already hold the derived xpriv, such as
/// the swap daemon. The legacy conversion keeps the pre-1.0 migration path on
/// the exact same key.
#[derive(Debug, Clone, Copy)]
pub struct StaticSeed {
    xpriv: Xpriv,
}

impl StaticSeed {
    pub fn new(xpriv: Xpriv) -> Self {
        Self { xpriv }
    }
}

impl BitcoinWalletSeed for StaticSeed {
    fn derive_extended_private_key(&self, _network: bitcoin::Network) -> Result<Xpriv> {
        Ok(self.xpriv)
    }

    fn derive_extended_private_key_legacy(
        &self,
        network: bdk::bitcoin::Network,
    ) -> Result<bdk::bitcoin::util::bip32::ExtendedPrivKey> {
        let child_number = match self.xpriv.child_number {
            bitcoin::bip32::ChildNumber::Normal { index } => {
                bdk::bitcoin::util::bip32::ChildNumber::from_normal_idx(index)
                    .context("Invalid legacy normal child number")?
            }
            bitcoin::bip32::ChildNumber::Hardened { index } => {
                bdk::bitcoin::util::bip32::ChildNumber::from_hardened_idx(index)
                    .context("Invalid legacy hardened child number")?
            }
        };

        Ok(bdk::bitcoin::util::bip32::ExtendedPrivKey {
            network,
            depth: self.xpriv.depth,
            parent_fingerprint: bdk::bitcoin::util::bip32::Fingerprint::from(
                &self.xpriv.parent_fingerprint.to_bytes()[..],
            ),
            child_number,
            private_key: bdk::bitcoin::secp256k1::SecretKey::from_slice(
                &self.xpriv.private_key.secret_bytes(),
            )
            .context("Invalid legacy private key")?,
            chain_code: bdk::bitcoin::util::bip32::ChainCode::from(
                &self.xpriv.chain_code.to_bytes()[..],
            ),
        })
    }
}

#[cfg(test)]
mod sync_deadline_tests {
    use super::*;

    #[test]
    fn the_sync_deadline_leaves_room_for_a_normal_tor_sync() {
        let config = Client::balancer_config(Some("127.0.0.1:9050".to_string()));
        assert!(
            config.call_timeout >= 120,
            "a full wallet sync takes about 60 s over Tor; a 60 s cap kills and restarts it forever"
        );
        assert_eq!(config.socks5_proxy.as_deref(), Some("127.0.0.1:9050"));
    }
}

#[cfg(test)]
mod sync_window_tests {
    use super::*;

    #[test]
    fn the_window_follows_the_highest_of_used_and_known_plus_the_gap() {
        assert_eq!(sync_window_end(None, 0), SYNC_WINDOW_GAP);
        assert_eq!(sync_window_end(Some(45), 0), 46 + SYNC_WINDOW_GAP);
        assert_eq!(sync_window_end(Some(3), 46), 46 + SYNC_WINDOW_GAP);
        assert_eq!(sync_window_end(Some(100), 46), 101 + SYNC_WINDOW_GAP);
    }

    #[test]
    fn an_unseeded_window_does_not_limit_the_sync() {
        assert_eq!(SyncWindow::default().floors(), None);
        let window = SyncWindow::default();
        window.set(46, 6);
        assert_eq!(window.floors(), Some((46, 6)));
    }

    // The deposit wallet database had 676 revealed scripts per keychain for real
    // indices of 46 and 6. After seeding with the real indices only the window
    // around them is synced.
    #[tokio::test]
    async fn a_bloated_wallet_syncs_only_the_window_once_seeded() {
        let wallet = TestWalletBuilder::new(100_000).build().await;
        wallet.seed_address_cache(46, 6, 675).await.unwrap();
        let revealed = wallet.wallet.lock().await.spk_index().revealed_spks(..).count();
        assert!(revealed >= 2 * 676, "revealed only {revealed} scripts");

        let count = wallet.sync_script_count().await;
        // external: max(used + 1, 46) + 20 = 66; internal: max(used + 1, 6) + 20 = 26
        assert!(count <= 66 + 26, "synced {count} scripts");
        assert!(count >= 66, "synced only {count} scripts");
    }

    // Addresses are handed out with `next_unused_address`; whatever it returns
    // must be inside the window or a deposit to it would never be seen.
    #[tokio::test]
    async fn handed_out_addresses_stay_inside_the_window() {
        let wallet = TestWalletBuilder::new(100_000).build().await;
        wallet.seed_address_cache(46, 6, 675).await.unwrap();

        let deposit = wallet.new_address().await.unwrap();
        let change = wallet.new_change_address().await.unwrap();

        let guard = wallet.wallet.lock().await;
        let in_window: std::collections::HashSet<ScriptBuf> = wallet
            .window_spks(&guard)
            .into_iter()
            .map(|(_, spk)| spk)
            .collect();
        assert!(in_window.contains(&deposit.script_pubkey()));
        assert!(in_window.contains(&change.script_pubkey()));
    }
}

#[cfg(test)]
mod mempool_refusal_tests {
    use super::*;

    /// Review T-5: the mempool.space client has no proxy, so it must never be built.
    #[test]
    fn the_clearnet_mempool_client_cannot_be_built_for_any_network() {
        for network in [
            bitcoin::Network::Bitcoin,
            bitcoin::Network::Testnet,
            bitcoin::Network::Signet,
            bitcoin::Network::Regtest,
        ] {
            let error = mempool_client::MempoolClient::new(network).err();
            assert!(
                error.is_some_and(|e| e.to_string().contains("never calls clearnet")),
                "{network:?}"
            );
        }
    }
}

/// Every full transaction the wallet stores, for seeding the Electrum client's cache.
fn known_transactions(wallet: &bdk_wallet::Wallet) -> Vec<Arc<bitcoin::Transaction>> {
    wallet.tx_graph().full_txs().map(|node| node.tx).collect()
}

#[cfg(test)]
mod counting_electrum;

#[cfg(test)]
mod sync_cache_tests {
    use super::*;
    use bdk_wallet::bitcoin::bip32::Xpriv;
    use bdk_wallet::bitcoin::Network as TestNetwork;
    use bitcoin::hashes::Hash as _;

    fn test_wallet() -> bdk_wallet::Wallet {
        let xprv = Xpriv::new_master(TestNetwork::Testnet4, &[7u8; 64]).unwrap();
        let build = |keychain| {
            SamouraiBip84(xprv, keychain, WalletAccount::Deposit)
                .build(TestNetwork::Testnet4)
                .unwrap()
        };
        bdk_wallet::Wallet::create(build(KeychainKind::External), build(KeychainKind::Internal))
            .network(TestNetwork::Testnet4)
            .create_wallet_no_persist()
            .unwrap()
    }

    fn payment_to(wallet: &bdk_wallet::Wallet, index: u32, sats: u64) -> bitcoin::Transaction {
        bitcoin::Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            // a distinct, non-null previous output (a null one makes BDK treat it as a coinbase)
            input: vec![bitcoin::TxIn {
                previous_output: bitcoin::OutPoint::new(
                    bitcoin::Txid::from_byte_array([index as u8 + 1; 32]),
                    0,
                ),
                ..Default::default()
            }],
            output: vec![bitcoin::TxOut {
                value: bitcoin::Amount::from_sat(sats),
                script_pubkey: wallet
                    .peek_address(KeychainKind::External, index)
                    .address
                    .script_pubkey(),
            }],
        }
    }

    /// A wallet with `stored` transactions already in its database, and a fake server whose
    /// history has those plus `unknown` more that the wallet has never seen.
    fn wallet_and_server(
        stored: u32,
        unknown: u32,
    ) -> (bdk_wallet::Wallet, super::counting_electrum::CountingElectrum) {
        use bdk_electrum::electrum_client::GetHistoryRes;

        let mut wallet = test_wallet();
        let mut server = super::counting_electrum::CountingElectrum::default();
        let mut additions = Vec::new();
        for index in 0..stored + unknown {
            let tx = payment_to(&wallet, index, 50_000 + u64::from(index));
            let txid = tx.compute_txid();
            server
                .raw_txs
                .insert(txid, bitcoin::consensus::encode::serialize(&tx));
            server.history.insert(
                tx.output[0].script_pubkey.as_bytes().to_vec(),
                vec![GetHistoryRes {
                    height: 0,
                    tx_hash: txid,
                    fee: None,
                }],
            );
            if index < stored {
                additions.push((tx, 10 + u64::from(index)));
            }
        }
        wallet.apply_unconfirmed_txs(additions);
        // the sync asks the server about every revealed script
        wallet
            .reveal_addresses_to(KeychainKind::External, stored + unknown)
            .for_each(drop);
        (wallet, server)
    }

    /// Run the real BDK Electrum sync against the fake server and report how many
    /// transactions it downloaded.
    fn transactions_downloaded(stored: u32, unknown: u32, seed_cache: bool) -> usize {
        use std::sync::atomic::Ordering;

        let (wallet, server) = wallet_and_server(stored, unknown);
        let fetches = server.transaction_fetches.clone();
        let client = bdk_electrum::BdkElectrumClient::new(server);
        if seed_cache {
            client.populate_tx_cache(known_transactions(&wallet).iter().cloned());
        }
        // The scripts the wallet would ask about. No chain tip is requested: that would need a
        // fake chain of block headers, and the cache behaviour does not depend on it.
        let scripts: Vec<_> = (0..stored + unknown)
            .map(|index| {
                wallet
                    .peek_address(KeychainKind::External, index)
                    .address
                    .script_pubkey()
            })
            .collect();
        let request = bdk_chain::spk_client::SyncRequest::builder()
            .spks(scripts)
            .build();
        client.sync(request, 32, false).expect("sync");
        fetches.load(Ordering::SeqCst)
    }

    // The slowness in numbers (see notes/reviews/2026-11-09-slowness-history.md): without a
    // seeded cache every stored transaction is downloaded again, one request each, at every
    // process start. 59 of them at ~0.75 s a round trip is the 60-70 s sync.
    #[test]
    fn an_unseeded_sync_downloads_every_stored_transaction_again() {
        assert_eq!(transactions_downloaded(12, 0, false), 12);
    }

    #[test]
    fn a_seeded_sync_downloads_none_of_the_stored_transactions() {
        assert_eq!(transactions_downloaded(12, 0, true), 0);
    }

    #[test]
    fn a_seeded_sync_still_downloads_transactions_the_wallet_has_not_seen() {
        assert_eq!(transactions_downloaded(12, 3, true), 3);
    }

    /// Source guard: the sync call must seed the client's cache before `client.sync`, or every
    /// process start downloads the whole transaction history one request at a time again
    /// (86 round trips, 60-70 s, growing with every swap). Mutation-checked.
    #[test]
    fn the_sync_call_seeds_the_cache_before_syncing() {
        let source = include_str!("wallet.rs");
        let call = source.find("\"sync_wallet\"").expect("sync_wallet call");
        let tail = &source[call..];
        let seed = tail.find("client.populate_tx_cache(").expect("cache is seeded");
        let sync = tail.find("client.sync(").expect("client.sync call");
        assert!(seed < sync, "the cache must be seeded before the sync");
    }

    #[test]
    fn an_empty_wallet_has_nothing_to_seed() {
        assert!(known_transactions(&test_wallet()).is_empty());
    }

    // The sync seeds the Electrum client's cache with these, so none of them is downloaded again.
    #[test]
    fn every_stored_transaction_is_handed_to_the_sync_cache() {
        let mut wallet = test_wallet();
        let first = payment_to(&wallet, 0, 50_000);
        let second = payment_to(&wallet, 1, 60_000);
        wallet.apply_unconfirmed_txs(vec![(first.clone(), 10), (second.clone(), 11)]);

        let mut known: Vec<_> = known_transactions(&wallet)
            .iter()
            .map(|tx| tx.compute_txid())
            .collect();
        known.sort();
        let mut expected = vec![first.compute_txid(), second.compute_txid()];
        expected.sort();
        assert_eq!(known, expected);
    }
}

/// Audit H-4: a confirmation counts only with a merkle proof against a header
/// that has real proof of work. Fixtures are real: testnet4 block 160302 and
/// its transaction at position 1, read from the project's own Fulcrum, and the
/// mainnet genesis block from the `bitcoin` crate.
#[cfg(test)]
mod inclusion_proof_tests {
    use super::*;
    use bdk_electrum::electrum_client::GetMerkleRes;
    use bitcoin::block::{Header, Version};
    use bitcoin::hashes::Hash;
    use bitcoin::hex::FromHex;
    use bitcoin::{CompactTarget, TxMerkleNode};
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Write};
    use std::str::FromStr;

    const HEIGHT: u32 = 160302;
    const HEADER: &str = "0060562b3a950cf3b4b06c396a8bdc416d4905b89158df5c54a59245a18b530a000000008668638272b409bd62870968a73f6fa35dbe9f5adf76f5d18ac9a85a1339f00922aef26aef6a04192c49b565";
    /// Testnet4 block 160300: real, but not the block holding the transaction.
    const OTHER_HEADER: &str = "00000120ddb0ae9dd9fb4395494850a723ec1fd69f1713d239dc0f9f0200000000000000fced26372c4a0d85f99ca984d7b1dfccc81874a926b5cdd00109f0995685358768aef26affff001d05974d0d";
    const TXID: &str = "feaba9804e4530a35adaa445385b20f15852d31407166017896ae27fdf39b7e7";
    const BRANCH: [&str; 3] = [
        "55852b77842078f50e633ecc21e0d4da47f7617398513038786ac7d660e5a67c",
        "a8fdaba5d4b43bafdb2d43e523584d11a99d55f5d378582c52af9d46cde9b1de",
        "1dd691572ac6fe2359563b5ef7a7e2717a700c6773486f1b373e68e8503582ff",
    ];

    fn header(hex: &str) -> Header {
        bitcoin::consensus::deserialize(&Vec::<u8>::from_hex(hex).unwrap()).unwrap()
    }

    fn txid() -> Txid {
        Txid::from_str(TXID).unwrap()
    }

    /// The proof as an Electrum server sends it.
    fn proof_json(pos: usize, branch: &[&str]) -> Value {
        json!({ "block_height": HEIGHT, "pos": pos, "merkle": branch })
    }

    fn proof(pos: usize, branch: &[&str]) -> GetMerkleRes {
        serde_json::from_value(proof_json(pos, branch)).unwrap()
    }

    /// A header a lying server could make up in moments: it commits to `txid`
    /// alone and carries the easiest possible proof of work.
    fn forged_header(txid: Txid) -> Header {
        let mut header = Header {
            version: Version::from_consensus(0x2000_0000),
            prev_blockhash: bitcoin::BlockHash::all_zeros(),
            merkle_root: TxMerkleNode::from_raw_hash(txid.to_raw_hash()),
            time: 1_700_000_000,
            bits: CompactTarget::from_consensus(0x207f_ffff),
            nonce: 0,
        };
        while header.validate_pow(header.target()).is_err() {
            header.nonce += 1;
        }
        header
    }

    fn mainnet_genesis() -> (Txid, Header) {
        let block = bitcoin::blockdata::constants::genesis_block(Network::Bitcoin);
        (block.txdata[0].compute_txid(), block.header)
    }

    // ---- the proof check ----

    #[test]
    fn a_real_proof_against_a_real_header_is_accepted() {
        verify_inclusion(&txid(), &proof(1, &BRANCH), &header(HEADER), None).unwrap();
    }

    #[test]
    fn a_proof_for_another_transaction_is_refused() {
        let other = Txid::from_byte_array([7u8; 32]);
        let error = verify_inclusion(&other, &proof(1, &BRANCH), &header(HEADER), None).unwrap_err();
        assert!(error.to_string().contains("merkle proof"), "{error:#}");
    }

    #[test]
    fn a_tampered_branch_is_refused() {
        let mut tampered = BRANCH;
        tampered[1] = "a8fdaba5d4b43bafdb2d43e523584d11a99d55f5d378582c52af9d46cde9b1df";
        assert!(verify_inclusion(&txid(), &proof(1, &tampered), &header(HEADER), None).is_err());
    }

    #[test]
    fn a_wrong_position_is_refused() {
        assert!(verify_inclusion(&txid(), &proof(0, &BRANCH), &header(HEADER), None).is_err());
        assert!(verify_inclusion(&txid(), &proof(3, &BRANCH), &header(HEADER), None).is_err());
    }

    #[test]
    fn a_real_header_of_another_block_is_refused() {
        assert!(verify_inclusion(&txid(), &proof(1, &BRANCH), &header(OTHER_HEADER), None).is_err());
    }

    #[test]
    fn a_header_that_misses_its_own_proof_of_work_is_refused() {
        let mut altered = header(HEADER);
        altered.nonce = altered.nonce.wrapping_add(1);
        let error = verify_inclusion(&txid(), &proof(1, &BRANCH), &altered, None).unwrap_err();
        assert!(error.to_string().contains("proof-of-work"), "{error:#}");
    }

    #[test]
    fn a_forged_header_with_trivial_work_is_refused_on_mainnet() {
        let claimed = Txid::from_byte_array([9u8; 32]);
        let forged = forged_header(claimed);
        let error = verify_inclusion(
            &claimed,
            &proof(0, &[]),
            &forged,
            min_header_difficulty(Network::Bitcoin),
        )
        .unwrap_err();
        assert!(error.to_string().contains("below the accepted minimum"), "{error:#}");
    }

    /// The known limit: test networks have no difficulty floor, so there a
    /// server willing to mine a trivial header can still forge a proof.
    #[test]
    fn test_networks_have_no_difficulty_floor() {
        let claimed = Txid::from_byte_array([9u8; 32]);
        for network in [Network::Testnet, Network::Signet, Network::Regtest] {
            assert_eq!(min_header_difficulty(network), None);
        }
        verify_inclusion(&claimed, &proof(0, &[]), &forged_header(claimed), None).unwrap();
    }

    #[test]
    fn the_mainnet_floor_compares_real_difficulty() {
        // The genesis block carries real proof of work at difficulty 1
        let (coinbase, genesis) = mainnet_genesis();
        verify_inclusion(&coinbase, &proof(0, &[]), &genesis, None).unwrap();
        verify_inclusion(&coinbase, &proof(0, &[]), &genesis, Some(1)).unwrap();
        assert!(verify_inclusion(&coinbase, &proof(0, &[]), &genesis, Some(2)).is_err());
        assert!(
            verify_inclusion(&coinbase, &proof(0, &[]), &genesis, min_header_difficulty(Network::Bitcoin))
                .is_err()
        );
        assert_eq!(
            min_header_difficulty(Network::Bitcoin),
            Some(MAINNET_MIN_HEADER_DIFFICULTY)
        );
    }

    // ---- judging several servers' answers ----

    fn answered(message: &str) -> bdk_electrum::electrum_client::Error {
        bdk_electrum::electrum_client::Error::Protocol(json!({"code": 1, "message": message}))
    }

    fn unreachable() -> bdk_electrum::electrum_client::Error {
        bdk_electrum::electrum_client::Error::IOError(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out",
        ))
    }

    #[test]
    fn one_valid_proof_outweighs_any_number_of_bad_ones() {
        let verdict = judge_inclusion_proofs(
            &txid(),
            vec![
                Ok((proof(0, &BRANCH), header(HEADER))),
                Err(answered("tx not in block")),
                Err(unreachable()),
                Ok((proof(1, &BRANCH), header(HEADER))),
            ],
            None,
        )
        .unwrap();
        assert_eq!(verdict, InclusionVerdict::Proven);
    }

    #[test]
    fn bad_proofs_and_refusals_refute_the_claim() {
        let verdict = judge_inclusion_proofs(
            &txid(),
            vec![
                Ok((proof(1, &BRANCH), header(OTHER_HEADER))),
                Err(answered("tx not in block")),
            ],
            None,
        )
        .unwrap();
        assert!(matches!(verdict, InclusionVerdict::Refuted(_)), "{verdict:?}");

        let only_refusals =
            judge_inclusion_proofs(&txid(), vec![Err(answered("tx not in block"))], None).unwrap();
        assert!(matches!(only_refusals, InclusionVerdict::Refuted(_)));
    }

    #[test]
    fn no_answer_at_all_is_an_error_so_the_caller_retries() {
        assert!(judge_inclusion_proofs(&txid(), vec![Err(unreachable())], None).is_err());
        assert!(judge_inclusion_proofs(&txid(), vec![], None).is_err());
    }

    #[test]
    fn a_transaction_with_a_different_txid_is_ignored() {
        let (_, genesis) = mainnet_genesis();
        let _ = genesis;
        let block = bitcoin::blockdata::constants::genesis_block(Network::Bitcoin);
        let real = block.txdata[0].clone();
        let mut impostor = real.clone();
        impostor.lock_time = bitcoin::absolute::LockTime::from_consensus(1);
        let wanted = real.compute_txid();

        let answers: Vec<std::result::Result<Option<Transaction>, ()>> =
            vec![Ok(Some(impostor.clone())), Ok(None), Ok(Some(real.clone()))];
        assert_eq!(matching_transaction(&wanted, &answers), Some(&real));

        let only_impostor: Vec<std::result::Result<Option<Transaction>, ()>> =
            vec![Ok(Some(impostor))];
        assert_eq!(matching_transaction(&wanted, &only_impostor), None);
    }

    // ---- against a real Electrum connection ----

    /// What one fake Electrum server says.
    #[derive(Clone)]
    struct FakeServer {
        /// (txid, height) for every script; height 0 means mempool.
        history: Vec<(Txid, i32)>,
        /// The merkle proof answer, or None to answer "not in block".
        proof: Option<Value>,
        header_hex: String,
        tip: u32,
    }

    fn respond(server: &FakeServer, request: &Value) -> Value {
        let id = request["id"].clone();
        let params = &request["params"];
        let result = match request["method"].as_str().unwrap_or("") {
            "server.version" => json!(["fake", "1.4"]),
            "server.ping" => Value::Null,
            "blockchain.scripthash.get_history" => Value::Array(
                server
                    .history
                    .iter()
                    .map(|(txid, height)| json!({"tx_hash": txid.to_string(), "height": height}))
                    .collect(),
            ),
            "blockchain.headers.subscribe" => {
                json!({"height": server.tip, "hex": server.header_hex})
            }
            "blockchain.block.header" => json!(server.header_hex),
            "blockchain.transaction.get_merkle" => match (&server.proof, params[1].as_u64()) {
                (Some(proof), Some(_)) => proof.clone(),
                _ => {
                    return json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": 1, "message": "tx not in block"}});
                }
            },
            other => {
                return json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32601, "message": format!("unknown method {other}")}});
            }
        };
        json!({"jsonrpc": "2.0", "id": id, "result": result})
    }

    /// Serve newline-delimited JSON-RPC (single and batched) on a local port.
    fn spawn_electrum(server: FakeServer) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let server = server.clone();
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { return };
                        let Ok(request) = serde_json::from_str::<Value>(&line) else { continue };
                        let reply = match &request {
                            Value::Array(batch) => {
                                Value::Array(batch.iter().map(|r| respond(&server, r)).collect())
                            }
                            single => respond(&server, single),
                        };
                        if writeln!(writer, "{reply}").is_err() {
                            return;
                        }
                    }
                });
            }
        });
        format!("tcp://{address}")
    }

    fn honest() -> FakeServer {
        FakeServer {
            history: vec![(txid(), HEIGHT as i32)],
            proof: Some(proof_json(1, &BRANCH)),
            header_hex: HEADER.to_string(),
            tip: HEIGHT + 4,
        }
    }

    async fn status_from(urls: Vec<String>, watched: Txid, network: Network) -> ScriptStatus {
        let client = Client::new(&urls, Duration::from_secs(60), None, network)
            .await
            .unwrap();
        let script = ScriptBuf::from_bytes(vec![0x51]);
        client.status_of_script(&(watched, script), true).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_honest_server_with_a_valid_proof_confirms() {
        let status = status_from(vec![spawn_electrum(honest())], txid(), Network::Testnet).await;
        assert_eq!(status, ScriptStatus::Confirmed(Confirmed::new(4)));
    }

    /// The audit's maker scenario: a server claims the buyer's lock is
    /// confirmed when it was never mined, and cannot prove it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_claiming_an_unmined_transaction_is_not_believed() {
        let never_mined = Txid::from_byte_array([7u8; 32]);
        let liar = FakeServer {
            history: vec![(never_mined, HEIGHT as i32)],
            proof: None,
            ..honest()
        };
        let status = status_from(vec![spawn_electrum(liar)], never_mined, Network::Testnet).await;
        assert_eq!(status, ScriptStatus::InMempool);
    }

    /// One lying server in the pool used to win because the highest height
    /// was taken. Now its claim needs a proof that no server can give.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_lying_server_in_the_pool_cannot_outvote_the_honest_one() {
        let unmined = Txid::from_byte_array([7u8; 32]);
        let liar = FakeServer {
            history: vec![(unmined, HEIGHT as i32)],
            proof: Some(proof_json(1, &BRANCH)),
            ..honest()
        };
        let truthful = FakeServer {
            history: vec![(unmined, 0)],
            proof: None,
            ..honest()
        };
        let status = status_from(
            vec![spawn_electrum(liar), spawn_electrum(truthful)],
            unmined,
            Network::Testnet,
        )
        .await;
        assert_eq!(status, ScriptStatus::InMempool);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_forged_header_does_not_confirm_on_mainnet() {
        let claimed = Txid::from_byte_array([9u8; 32]);
        let forger = FakeServer {
            history: vec![(claimed, HEIGHT as i32)],
            proof: Some(json!({"block_height": HEIGHT, "pos": 0, "merkle": []})),
            header_hex: bitcoin::consensus::encode::serialize_hex(&forged_header(claimed)),
            tip: HEIGHT,
        };
        let status = status_from(vec![spawn_electrum(forger)], claimed, Network::Bitcoin).await;
        assert_eq!(status, ScriptStatus::InMempool);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_inclusion_check_used_for_the_dojo_proves_and_refutes() {
        let url = spawn_electrum(honest());
        let client = Client::new(&[url], Duration::from_secs(60), None, Network::Testnet)
            .await
            .unwrap();

        assert_eq!(
            client.verify_inclusion_at(txid(), HEIGHT).await.unwrap(),
            InclusionVerdict::Proven
        );
        // A different transaction at the same block is not proven by its branch
        assert!(matches!(
            client
                .verify_inclusion_at(Txid::from_byte_array([7u8; 32]), HEIGHT)
                .await
                .unwrap(),
            InclusionVerdict::Refuted(_)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_proven_inclusion_is_remembered() {
        let url = spawn_electrum(honest());
        let client = Client::new(&[url], Duration::from_secs(60), None, Network::Testnet)
            .await
            .unwrap();
        client.verify_inclusion_at(txid(), HEIGHT).await.unwrap();
        assert!(client.verified_inclusions.lock().unwrap().contains(&(txid(), HEIGHT)));
    }
}
