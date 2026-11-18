use crate::env::Config;
use crate::monero::{
    Amount, InsufficientFunds, PrivateViewKey, PublicViewKey, TransferProof, TxHash,
};
use crate::util;
use ::monero::{Address, Network, PrivateKey, PublicKey};
use anyhow::{Context, Result};
use jni::JNIEnv;
use monero_rpc::wallet::{BlockHeight, GetTransfers, MoneroWalletRpc as _, Refreshed};
use monero_rpc::{jsonrpc, wallet};
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Interval;
use url::Url;

/// Spacing applied between Monero lock constructions through a shared wallet.
///
/// Port of the eigenwallet 4.15.0 fix for concurrent-swap Monero double-spends:
/// when multiple swaps start at the same time, each must construct its lock
/// transaction in its own window (the wallet-rpc output selection can otherwise
/// pick the same outputs for two swaps, so only one succeeds).
///
/// Divergence from eigenwallet documented: their defaults are 5 minutes for
/// mainnet AND stagenet, 5 seconds for testnet; we keep stagenet at 5 seconds
/// so dev/test loops stay fast (stagenet is our GUI test mode). The cooldown is
/// not user-configurable in the GUI.
pub const MONERO_LOCK_CONSTRUCTION_COOLDOWN_MAINNET: Duration = Duration::from_secs(5 * 60);
pub const MONERO_LOCK_CONSTRUCTION_COOLDOWN_TEST: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct ConstructionThrottle {
    next_allowed: tokio::sync::Mutex<tokio::time::Instant>,
    interval: Duration,
}

impl ConstructionThrottle {
    pub fn new(interval: Duration) -> Result<Self> {
        anyhow::ensure!(
            !interval.is_zero(),
            "Construction turn duration must be positive"
        );
        Ok(Self {
            next_allowed: tokio::sync::Mutex::new(tokio::time::Instant::now()),
            interval,
        })
    }

    /// Waits until the caller's construction slot, then books the next slot for
    /// the following caller. Returns the interval so the caller can enforce that
    /// its construction finished inside the booked window if it wants to.
    pub async fn wait_for_my_turn(&self) -> Duration {
        let mut next_allowed = self.next_allowed.lock().await;
        tokio::time::sleep_until(*next_allowed).await;
        *next_allowed = tokio::time::Instant::now() + self.interval;
        self.interval
    }
}

#[derive(Debug)]
pub struct Wallet {
    pub inner: Mutex<wallet::Client>,
    pub network: Network,
    pub name: String,
    pub main_address: monero::Address,
    pub sync_interval: Duration,
    pub construction_throttle: ConstructionThrottle,
}

impl Wallet {
    /// Connect to a wallet RPC and load the given wallet by name.
    pub async fn open_or_create(url: Url, name: String, env_config: Config) -> Result<Self> {
        let client = wallet::Client::new(url)?;

        match client.open_wallet(name.clone()).await {
            Err(error) => {
                tracing::debug!(%error, "Open wallet response error");
                client.create_wallet(name.clone(), "English".to_owned()).await.context(
                    "Unable to create Monero wallet, please ensure that the monero-wallet-rpc is available",
                )?;

                tracing::debug!(monero_wallet_name = %name, "Created Monero wallet");
            }
            Ok(_) => tracing::debug!(monero_wallet_name = %name, "Opened Monero wallet"),
        }

        Self::connect(client, name, env_config).await
    }

    /// Open an existing wallet by name without creating or scanning a new one.
    pub async fn open_existing(url: Url, name: String, env_config: Config) -> Result<Self> {
        let client = wallet::Client::new(url)?;
        client.open_wallet(name.clone()).await?;
        Self::connect(client, name, env_config).await
    }

    /// Create/open a monitoring wallet without scanning from genesis.
    ///
    /// The Bob monitoring wallet is only used for `check_tx_key`; it does not
    /// need to own the shared lock address. A normal `create_wallet` starts at
    /// height 0 and blocks the single-threaded wallet-RPC while it scans the
    /// whole chain. Generating a placeholder wallet at a future height keeps
    /// the RPC responsive and finality checks functional.
    pub async fn open_or_create_at_height(
        url: Url,
        name: String,
        env_config: Config,
        restore_height: u32,
    ) -> Result<Self> {
        // The wallet RPC refuses connections for a few seconds after it is
        // launched. Retry connection-level failures instead of failing the
        // swap (which was only reported after the parallel Bitcoin sync).
        let mut attempt = 1u32;
        loop {
            match Self::open_or_create_at_height_once(
                url.clone(),
                name.clone(),
                env_config,
                restore_height,
            )
            .await
            {
                Err(error)
                    if attempt < MONITORING_WALLET_CONNECT_ATTEMPTS
                        && is_wallet_rpc_connect_error(&format!("{error:#}")) =>
                {
                    tracing::warn!(
                        attempt,
                        "Monero wallet RPC is not accepting connections yet; retrying: {error:#}"
                    );
                    tokio::time::sleep(MONITORING_WALLET_CONNECT_RETRY_DELAY).await;
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    async fn open_or_create_at_height_once(
        url: Url,
        name: String,
        env_config: Config,
        restore_height: u32,
    ) -> Result<Self> {
        let client = wallet::Client::new(url)?;

        match client.open_wallet(name.clone()).await {
            Err(error) => {
                tracing::debug!(%error, "Open wallet response error");
                let spend = PrivateKey::from_slice(&[1u8; 32])
                    .context("Invalid placeholder Monero spend key")?;
                let view = PrivateKey::from_slice(&[2u8; 32])
                    .context("Invalid placeholder Monero view key")?;
                let public_spend = PublicKey::from_private_key(&spend);
                let public_view = PublicKey::from_private_key(&view);
                let address =
                    Address::standard(env_config.monero_network, public_spend, public_view);
                client
                    .generate_from_keys(
                        name.clone(),
                        address.to_string(),
                        spend.to_string(),
                        view.to_string(),
                        restore_height,
                        String::new(),
                        true,
                    )
                    .await
                    .context("Unable to create height-bounded Monero monitoring wallet")?;
                tracing::debug!(
                    monero_wallet_name = %name,
                    restore_height,
                    "Created height-bounded Monero monitoring wallet"
                );
            }
            Ok(_) => tracing::debug!(monero_wallet_name = %name, "Opened Monero wallet"),
        }

        Self::connect(client, name, env_config).await
    }

    /// Connects to a wallet RPC where a wallet is already loaded.
    pub async fn connect(client: wallet::Client, name: String, env_config: Config) -> Result<Self> {
        let main_address =
            monero::Address::from_str(client.get_address(0).await?.address.as_str())?;
        // The cooldown is an anti-double-spend guard for real funds. Regtest
        // maps Monero to Mainnet for address compatibility, so keying only
        // off the Monero network would impose the five-minute mainnet slot on
        // a chain whose cancel timelock is already shorter than that.
        let construction_cooldown = match env_config.bitcoin_network {
            bitcoin::Network::Bitcoin => MONERO_LOCK_CONSTRUCTION_COOLDOWN_MAINNET,
            bitcoin::Network::Testnet
            | bitcoin::Network::Testnet4
            | bitcoin::Network::Regtest
            | bitcoin::Network::Signet => MONERO_LOCK_CONSTRUCTION_COOLDOWN_TEST,
        };
        Ok(Self {
            inner: Mutex::new(client),
            network: env_config.monero_network,
            name,
            main_address,
            sync_interval: env_config.monero_sync_interval(),
            construction_throttle: ConstructionThrottle::new(construction_cooldown)?,
        })
    }

    /// Re-open the wallet using the internally stored name.
    pub async fn re_open(&self) -> Result<()> {
        self.inner
            .lock()
            .await
            .open_wallet(self.name.clone())
            .await?;
        Ok(())
    }

    pub async fn open(&self, filename: String) -> Result<()> {
        self.inner.lock().await.open_wallet(filename).await?;
        Ok(())
    }

    pub async fn close(&self) -> Result<()> {
        self.inner.lock().await.close_wallet().await?;
        Ok(())
    }

    pub async fn store(&self) -> Result<()> {
        self.inner.lock().await.store().await?;
        Ok(())
    }

    /// Close the wallet and open (load) another wallet by generating it from
    /// keys. The generated wallet will remain loaded.
    pub async fn create_from_and_load(
        &self,
        file_name: String,
        private_spend_key: PrivateKey,
        private_view_key: PrivateViewKey,
        restore_height: BlockHeight,
    ) -> Result<()> {
        let public_spend_key = PublicKey::from_private_key(&private_spend_key);
        let public_view_key = PublicKey::from_private_key(&private_view_key.into());

        let address = Address::standard(self.network, public_spend_key, public_view_key);

        let wallet = self.inner.lock().await;

        // Properly close the wallet before generating the other wallet to ensure that
        // it saves its state correctly
        let _ = wallet
            .close_wallet()
            .await
            .context("Failed to close wallet")?;

        let _ = wallet
            .generate_from_keys(
                file_name,
                address.to_string(),
                private_spend_key.to_string(),
                PrivateKey::from(private_view_key).to_string(),
                restore_height.height,
                String::from(""),
                true,
            )
            .await
            .context("Failed to generate new wallet from keys")?;

        Ok(())
    }

    /// Close the wallet and open (load) another wallet by generating it from
    /// keys. The generated wallet will be opened, all funds sweeped to the
    /// main_address and then the wallet will be re-loaded using the internally
    /// stored name.
    ///
    /// Fails if the generated wallet cannot be refreshed or swept: a swallowed
    /// error here used to strand the refunded funds in the swap's temporary
    /// wallet while the swap advanced to a terminal state (incident 2026-10-13,
    /// swap 2a779092).
    pub async fn create_from(
        &self,
        file_name: String,
        private_spend_key: PrivateKey,
        private_view_key: PrivateViewKey,
        restore_height: BlockHeight,
    ) -> Result<()> {
        create_from_and_sweep(
            &self.inner,
            self.network,
            &self.name,
            &self.main_address.to_string(),
            file_name,
            private_spend_key,
            private_view_key,
            restore_height,
        )
        .await
    }

    pub async fn transfer(&self, request: TransferRequest) -> Result<TransferProof> {
        // Serialize lock constructions across concurrent swaps sharing this wallet:
        // each caller waits for its slot before the wallet-rpc constructs and
        // broadcasts the transaction, preventing two swaps from selecting the same
        // outputs (eigenwallet 4.15.0 double-spend fix).
        self.construction_throttle.wait_for_my_turn().await;
        let inner = self.inner.lock().await;

        let TransferRequest {
            public_spend_key,
            public_view_key,
            amount,
        } = request;

        let destination_address =
            Address::standard(self.network, public_spend_key, public_view_key.into());

        let res = inner
            .transfer_single(0, amount.as_piconero(), &destination_address.to_string())
            .await?;

        tracing::debug!(
            %amount,
            to = %public_spend_key,
            tx_id = %res.tx_hash,
            "Successfully initiated Monero transfer"
        );

        Ok(TransferProof::new(
            TxHash(res.tx_hash),
            res.tx_key
                .context("Missing tx_key in `transfer` response")?,
        ))
    }

    /// Construct a multi-destination transaction without relaying it.
    ///
    /// This is the safe construct/publish split required by protocol 5.0:
    /// Alice must not broadcast the XMR lock before Bob's BTC lock is safe.
    pub async fn construct_transfer(
        &self,
        destinations: Vec<(Address, Amount)>,
    ) -> Result<ConstructedTx> {
        self.construction_throttle.wait_for_my_turn().await;

        let destinations = destinations
            .into_iter()
            .map(|(address, amount)| wallet::Destination {
                amount: amount.as_piconero(),
                address: address.to_string(),
            })
            .collect();

        let tx = self
            .inner
            .lock()
            .await
            .transfer_constructed(0, destinations)
            .await
            .context("Failed to construct Monero transaction")?;

        ConstructedTx::try_from(tx)
    }

    /// Relay a transaction previously returned by [`Self::construct_transfer`].
    pub async fn publish_constructed(&self, tx: &ConstructedTx) -> Result<TxHash> {
        if tx.tx_metadata.is_empty() {
            anyhow::bail!("Cannot publish a Monero transaction without tx metadata");
        }

        let relayed = self
            .inner
            .lock()
            .await
            .relay_transaction(tx.tx_metadata.clone())
            .await
            .context("Failed to relay Monero transaction")?;

        if relayed.tx_hash != tx.tx_hash.0 {
            anyhow::bail!(
                "Relayed Monero transaction hash {} does not match constructed hash {}",
                relayed.tx_hash,
                tx.tx_hash
            );
        }

        Ok(TxHash(relayed.tx_hash))
    }

    /// Whether the wallet has a transfer with this txid (outgoing, pending, or incoming).
    pub async fn transaction_present(&self, txid: &TxHash) -> Result<bool> {
        match self
            .inner
            .lock()
            .await
            .get_transfer_by_txid(txid.0.clone())
            .await
        {
            Ok(_) => Ok(true),
            Err(jsonrpc::Error::JsonRpc(jsonrpc::JsonRpcError { code: -8, .. })) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Wait until a txid has `target` confirmations, polling the wallet RPC.
    pub async fn wait_for_tx_confirmations(&self, txid: &TxHash, target: u64) -> Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30 * 60);

        loop {
            match self
                .inner
                .lock()
                .await
                .get_transfer_by_txid(txid.0.clone())
                .await
            {
                Ok(response) => {
                    let transfer = response.transfer;
                    if transfer.double_spend_seen {
                        anyhow::bail!("Monero transaction {txid} was double-spent");
                    }
                    if transfer.confirmations >= target {
                        return Ok(());
                    }
                }
                Err(jsonrpc::Error::JsonRpc(jsonrpc::JsonRpcError { code: -8, .. })) => {
                    tracing::debug!(%txid, "Monero transaction is not visible yet");
                }
                Err(error) => {
                    tracing::warn!(%txid, "Failed to look up Monero transaction: {:#}", error);
                }
            }

            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("Timed out waiting for Monero transaction {txid} to confirm");
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }

    /// Wallet-RPC health check used before starting a swap.
    pub async fn health_check(&self) -> Result<()> {
        self.inner.lock().await.get_version().await?;
        Ok(())
    }

    /// Unlocked balance of the primary account.
    pub async fn unlocked_balance(&self) -> Result<Amount> {
        let balance = self.inner.lock().await.get_balance(0).await?;
        Ok(Amount::from_piconero(balance.unlocked_balance))
    }

    /// Reconcile a possibly-broadcast transfer: look for an outgoing (or
    /// pending) transaction of exactly `request`'s amount to `request`'s
    /// destination. Returns its transfer proof (tx hash + tx key, fetched
    /// separately via `get_tx_key` - `get_transfers` entries do not carry
    /// the key) when found.
    ///
    /// Used around the XMR lock step: a `transfer` call that errors after
    /// broadcasting (e.g. timeout after submit) or a crashed process between
    /// broadcast and state-persist must be adopted, not double-sent.
    pub async fn find_outgoing_transfer(
        &self,
        request: &TransferRequest,
    ) -> Result<Option<TransferProof>> {
        let destination = destination_address(self.network, request);

        let txid = {
            let inner = self.inner.lock().await;
            let transfers = inner.get_transfers(true, true).await?;
            match match_outgoing_transfer(&transfers, request, &destination) {
                Some(txid) => txid,
                None => return Ok(None),
            }
        };

        let key = {
            let inner = self.inner.lock().await;
            inner.get_tx_key(txid.0.clone()).await.with_context(|| {
                format!(
                    "Outgoing transfer {} matched the lock request but its tx key is unavailable",
                    txid.0
                )
            })?
        };

        let tx_key = PrivateKey::from_str(&key.tx_key)
            .context("Recorded tx key is not a valid private key")?;

        Ok(Some(TransferProof::new(txid, tx_key)))
    }

    /// Whether this wallet has an outgoing (including pending) transfer to
    /// `address`. Used to recover a redeem sweep that was broadcast just before
    /// the process died, without blindly sweeping a second time.
    pub async fn has_outgoing_transfer_to(&self, address: Address) -> Result<bool> {
        let transfers = self.inner.lock().await.get_transfers(true, true).await?;
        Ok(transfers
            .out
            .values()
            .chain(transfers.pending.values())
            .any(|transfer| transfer_sends_to(transfer, &address.to_string())))
    }

    /// Wait until an outgoing redeem sweep to `address` has at least one
    /// confirmation. This keeps the redeem state non-terminal until the user's
    /// wallet actually has spendable XMR.
    pub async fn wait_for_outgoing_sweep_confirmation(&self, address: Address) -> Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30 * 60);
        let address = address.to_string();
        loop {
            let transfers = match self.inner.lock().await.get_transfers(true, true).await {
                Ok(transfers) => transfers,
                Err(error) => {
                    tracing::warn!(
                        "Failed to read outgoing redeem transfers; retrying: {:#}",
                        error
                    );
                    if tokio::time::Instant::now() >= deadline {
                        return Err(error.into());
                    }
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
            };
            let mut found = false;
            for transfer in transfers.out.values().chain(transfers.pending.values()) {
                if !transfer_sends_to(transfer, &address) {
                    continue;
                }
                found = true;
                if transfer.double_spend_seen {
                    anyhow::bail!("Redeem sweep transaction was double-spent");
                }
                if transfer.confirmations >= 1 {
                    return Ok(());
                }
            }

            if tokio::time::Instant::now() >= deadline {
                if found {
                    anyhow::bail!("Timed out waiting for the redeem sweep to confirm");
                }
                anyhow::bail!("No outgoing redeem sweep found while waiting for confirmation");
            }
            if !found {
                tracing::debug!(
                    "Redeem sweep not visible yet; waiting instead of treating it as missing"
                );
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }

    /// Wait until a previously persisted redeem-sweep txid has at least one
    /// confirmation. This closes the gap where `get_transfers` is temporarily
    /// unavailable after the wallet-rpc restart: the txid was persisted before
    /// the wait, so resume can query it directly instead of re-sweeping.
    pub async fn wait_for_outgoing_sweep_txid(&self, txid: TxHash) -> Result<()> {
        wait_for_outgoing_sweep_txid_with(
            &self.inner,
            txid,
            Duration::from_secs(10),
            Duration::from_secs(30 * 60),
        )
        .await
    }

    pub async fn watch_for_transfer(&self, request: WatchRequest) -> Result<(), InsufficientFunds> {
        self.watch_for_transfer_with_progress::<fn(&str, u64)>(request, None)
            .await
    }

    pub async fn watch_for_transfer_with_progress<F>(
        &self,
        request: WatchRequest,
        on_confirmation: Option<F>,
    ) -> Result<(), InsufficientFunds>
    where
        F: Fn(&str, u64),
    {
        let WatchRequest {
            conf_target,
            public_view_key,
            public_spend_key,
            transfer_proof,
            expected,
        } = request;

        let txid = transfer_proof.tx_hash();

        tracing::info!(
            %txid,
            target_confirmations = %conf_target,
            "Waiting for Monero transaction finality"
        );

        let address = Address::standard(self.network, public_spend_key, public_view_key.into());

        let check_interval = tokio::time::interval(self.sync_interval);

        wait_for_confirmations_with_progress(
            &self.inner,
            transfer_proof,
            address,
            expected,
            conf_target,
            check_interval,
            self.name.clone(),
            on_confirmation,
        )
        .await?;

        Ok(())
    }

    /// Verify a transfer proof against a destination address using wallet-RPC.
    pub async fn verify_transfer_to(
        &self,
        transfer_proof: &TransferProof,
        address: &Address,
        expected: Amount,
    ) -> Result<bool> {
        let response = self
            .inner
            .lock()
            .await
            .check_tx_key(
                transfer_proof.tx_hash().0,
                transfer_proof.tx_key().to_string(),
                address.to_string(),
            )
            .await?;

        Ok(response.received >= expected.as_piconero())
    }

    /// Generate the swap's redeem wallet from the recovered keys, sweep it to
    /// `destination`, then re-open the monitoring wallet.
    pub async fn redeem_from_keys(
        &self,
        file_name: String,
        private_spend_key: PrivateKey,
        private_view_key: PrivateViewKey,
        restore_height: BlockHeight,
        destination: Address,
        on_sync: impl FnOnce(),
        on_sweep: impl FnOnce(),
    ) -> Result<Vec<TxHash>> {
        let original_name = self.name.clone();

        if let Err(error) = self
            .create_from_and_load(
                file_name.clone(),
                private_spend_key,
                private_view_key,
                restore_height,
            )
            .await
        {
            tracing::warn!("Failed to generate redeem wallet from keys: {error:#}");
            self.inner.lock().await.open_wallet(file_name).await?;
        }

        on_sync();
        let result: Result<Vec<TxHash>> = async {
            retry_with_backoff(|| self.refresh())
                .await
                .context("Failed to sync the redeem wallet after retries")?;
            let balance = self.get_balance().await?;
            if balance.balance == 0 {
                tracing::info!("Redeem wallet holds no funds; the sweep was already completed");
                return Ok(Vec::new());
            }
            on_sweep();
            self.sweep_all(destination).await
        }
        .await;

        if let Err(error) = self
            .inner
            .lock()
            .await
            .open_wallet(original_name.clone())
            .await
        {
            tracing::warn!(
                wallet = %original_name,
                "Failed to re-open the monitoring wallet after the XMR redeem sweep: {error:#}"
            );
        }

        result
    }

    pub async fn sweep_all(&self, address: Address) -> Result<Vec<TxHash>> {
        let sweep_all = self
            .inner
            .lock()
            .await
            .sweep_all(address.to_string(), false, false, false, false)
            .await?;

        let tx_hashes = sweep_all.tx_hash_list.into_iter().map(TxHash).collect();
        Ok(tx_hashes)
    }

    /// Get the balance of the primary account.
    pub async fn get_balance(&self) -> Result<wallet::GetBalance> {
        Ok(self.inner.lock().await.get_balance(0).await?)
    }

    pub async fn block_height(&self) -> Result<BlockHeight> {
        Ok(self.inner.lock().await.get_height().await?)
    }

    pub fn get_main_address(&self) -> Address {
        self.main_address
    }

    pub async fn refresh(&self) -> Result<Refreshed> {
        Ok(self.inner.lock().await.refresh().await?)
    }

    /// Refresh first, then read the wallet height.
    ///
    /// `get_height()` returns the wallet's cached scan height, not the chain
    /// tip. Recording it before a refresh makes the redeem wallet start its
    /// scan potentially years in the past (legacy §188). Keep these two calls
    /// together so every restore-height caller preserves that ordering.
    pub async fn refresh_then_block_height(&self) -> Result<BlockHeight> {
        refresh_then_block_height(self).await
    }

    /// [`Self::refresh_then_block_height`], retried through transient wallet-RPC failures for
    /// `budget`. For a resumed swap whose Bitcoin is already locked, where the first
    /// "no connection to daemon" must not end the step.
    pub async fn refresh_then_block_height_retrying(
        &self,
        budget: Duration,
        on_retry: impl FnMut(u64, Duration, &anyhow::Error),
    ) -> Result<BlockHeight> {
        refresh_then_block_height_retrying_on(self, budget, on_retry).await
    }
}

#[async_trait::async_trait]
pub(crate) trait RestoreHeightWallet {
    async fn refresh_for_restore_height(&self) -> Result<()>;
    async fn block_height_for_restore_height(&self) -> Result<BlockHeight>;
}

#[async_trait::async_trait]
impl RestoreHeightWallet for Wallet {
    async fn refresh_for_restore_height(&self) -> Result<()> {
        self.refresh().await?;
        Ok(())
    }

    async fn block_height_for_restore_height(&self) -> Result<BlockHeight> {
        self.block_height().await
    }
}

/// How long the redeem sweep keeps retrying wallet-RPC failures before it
/// gives up and leaves the swap resumable.
const REDEEM_SYNC_RETRY_BUDGET: Duration = Duration::from_secs(60 * 60);
/// Initial and maximum delay between wallet-RPC retries.
const REDEEM_SYNC_RETRY_INITIAL: Duration = Duration::from_secs(5);
const REDEEM_SYNC_RETRY_MAX: Duration = Duration::from_secs(60);

/// Retries a wallet-RPC operation through transient failures with exponential
/// backoff until `budget` is exhausted.
///
/// The redeem sweep must survive a lost daemon connection: by the time it runs
/// Alice has already redeemed the BTC, the XMR is in the redeem wallet, and a
/// failure only strands it until a manual resume. A live swap hit exactly this
/// when Tor dropped for ~16 minutes (JSON-RPC -38, `no_connection_to_daemon`).
async fn retry_with_backoff<F, Fut, T>(operation: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    retry_with_backoff_for(
        "during the XMR redeem",
        REDEEM_SYNC_RETRY_BUDGET,
        operation,
        |_, _, _| {},
    )
    .await
}

/// [`retry_with_backoff`] for any wallet-RPC step: `what` names the step in the log and in the
/// final error, `budget` is how long to keep trying, and `on_retry(attempt, elapsed, error)` runs
/// before each wait (the resume path uses it to tell the user what is going on).
async fn retry_with_backoff_for<F, Fut, T, N>(
    what: &str,
    budget: Duration,
    mut operation: F,
    mut on_retry: N,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
    N: FnMut(u64, Duration, &anyhow::Error),
{
    // tokio's clock: the same as the system clock in the app, and tests with paused time can
    // run the whole budget at once
    let started = tokio::time::Instant::now();
    let mut delay = REDEEM_SYNC_RETRY_INITIAL;
    let mut attempt: u64 = 0;

    loop {
        attempt += 1;
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let elapsed = started.elapsed();
                if elapsed >= budget {
                    return Err(error).context(format!(
                        "Wallet RPC still unavailable {what} after {}s",
                        elapsed.as_secs()
                    ));
                }
                tracing::warn!(
                    attempt,
                    elapsed_seconds = elapsed.as_secs(),
                    retry_in_seconds = delay.as_secs(),
                    %error,
                    "Wallet RPC unavailable {what}; retrying"
                );
                on_retry(attempt, elapsed, &error);
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(REDEEM_SYNC_RETRY_MAX);
            }
        }
    }
}

/// How far before the chain tip the redeem wallet starts scanning when the
/// swap records its restore height. The XMR lock cannot exist yet at that
/// point, so the margin only absorbs reorgs and clock skew.
pub const REDEEM_RESTORE_HEIGHT_MARGIN: u64 = 10;

/// Picks the redeem wallet's restore height at swap setup.
///
/// The redeem wallet scans from this height when Alice's redeem is swept, so a
/// height that is too old turns the sweep into a multi-thousand-block scan
/// (WORKLOG §364), while a height after the XMR lock would miss the funds.
///
/// `tip` is the daemon's chain height; `recorded` is the wallet's cached
/// height, which for the check wallet is a hardcoded conservative constant.
/// A tip-based height is always safe here because the lock transaction is
/// created after this point.
pub fn redeem_restore_height(tip: Option<u64>, recorded: u64) -> u64 {
    match tip {
        Some(tip) => tip.saturating_sub(REDEEM_RESTORE_HEIGHT_MARGIN),
        None => recorded,
    }
}

async fn refresh_then_block_height<W>(wallet: &W) -> Result<BlockHeight>
where
    W: RestoreHeightWallet + ?Sized,
{
    wallet.refresh_for_restore_height().await?;
    wallet.block_height_for_restore_height().await
}

/// [`refresh_then_block_height`] that keeps trying through transient wallet-RPC failures
/// ("no connection to daemon") for `budget`, instead of failing on the first one.
async fn refresh_then_block_height_retrying_on<W, N>(
    wallet: &W,
    budget: Duration,
    on_retry: N,
) -> Result<BlockHeight>
where
    W: RestoreHeightWallet + ?Sized,
    N: FnMut(u64, Duration, &anyhow::Error),
{
    retry_with_backoff_for(
        "while reading the restore height",
        budget,
        || refresh_then_block_height(wallet),
        on_retry,
    )
    .await
}

#[derive(Clone, Debug)]
pub struct ConstructedTx {
    pub tx_hash: TxHash,
    pub tx_blob: String,
    pub tx_metadata: String,
    pub tx_key: Option<PrivateKey>,
    pub amount: Amount,
    pub fee: Amount,
}

impl ConstructedTx {
    pub fn transfer_proof(&self) -> Result<TransferProof> {
        let tx_key = self
            .tx_key
            .ok_or_else(|| anyhow::anyhow!("Constructed Monero transaction has no tx key"))?;
        Ok(TransferProof::new(self.tx_hash.clone(), tx_key))
    }
}

impl TryFrom<wallet::Transfer> for ConstructedTx {
    type Error = anyhow::Error;

    fn try_from(tx: wallet::Transfer) -> Result<Self> {
        if tx.tx_metadata.is_empty() {
            anyhow::bail!("wallet-rpc returned a constructed transaction without tx metadata");
        }

        Ok(Self {
            tx_hash: TxHash(tx.tx_hash),
            tx_blob: tx.tx_blob,
            tx_metadata: tx.tx_metadata,
            tx_key: tx.tx_key,
            amount: Amount::from_piconero(tx.amount),
            fee: Amount::from_piconero(tx.fee),
        })
    }
}

#[derive(Clone, Debug)]
pub struct TransferRequest {
    pub public_spend_key: PublicKey,
    pub public_view_key: PublicViewKey,
    pub amount: Amount,
}

#[derive(Debug)]
pub struct WatchRequest {
    pub public_spend_key: PublicKey,
    pub public_view_key: PublicViewKey,
    pub transfer_proof: TransferProof,
    pub conf_target: u64,
    pub expected: Amount,
}

fn destination_address(network: Network, request: &TransferRequest) -> Address {
    Address::standard(
        network,
        request.public_spend_key,
        request.public_view_key.into(),
    )
}

async fn wait_for_outgoing_sweep_txid_with<C, H>(
    client: &Mutex<C>,
    txid: TxHash,
    poll_interval: Duration,
    timeout: Duration,
) -> Result<()>
where
    C: wallet::MoneroWalletRpc<H> + Sync,
    H: Send + Sync + 'static + monero_rpc::jsonrpc::SendRequest,
    <H as monero_rpc::jsonrpc::SendRequest>::Error: std::error::Error + Send + Sync + 'static,
    monero_rpc::jsonrpc::Error<<H as monero_rpc::jsonrpc::SendRequest>::Error>:
        From<<H as monero_rpc::jsonrpc::SendRequest>::Error>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match client
            .lock()
            .await
            .get_transfer_by_txid(txid.0.clone())
            .await
        {
            Ok(response) => {
                let transfer = response.transfer;
                if transfer.double_spend_seen {
                    anyhow::bail!("Redeem sweep transaction was double-spent");
                }
                if transfer.confirmations >= 1 {
                    return Ok(());
                }
            }
            Err(jsonrpc::Error::JsonRpc(jsonrpc::JsonRpcError { code: -8, .. })) => {
                tracing::debug!(%txid, "Persisted redeem sweep txid is not visible yet");
            }
            Err(error) => {
                tracing::warn!(
                    %txid,
                    "Failed to look up the persisted redeem sweep; retrying: {:#}",
                    error
                );
            }
        }

        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("Timed out waiting for persisted redeem sweep {txid} to confirm");
        }
        tokio::time::sleep(poll_interval).await;
    }
}

fn transfer_sends_to(transfer: &monero_rpc::wallet::TransferInfo, target: &str) -> bool {
    transfer.address == target
        || transfer
            .destinations
            .iter()
            .any(|destination| destination.address == target)
}

/// Match the wallet's outgoing/pending transfer history against a transfer
/// request: exact amount to the exact destination address. Returns the
/// transaction's hash (the entry's own `txid` field, falling back to the
/// map key). The tx key is NOT part of the entry - it is fetched
/// separately via `get_tx_key`.
fn match_outgoing_transfer(
    transfers: &GetTransfers,
    request: &TransferRequest,
    destination: &Address,
) -> Option<TxHash> {
    transfers
        .out
        .iter()
        .chain(transfers.pending.iter())
        .find(|(_, transfer)| {
            transfer.amount == request.amount.as_piconero()
                && transfer_sends_to(transfer, &destination.to_string())
        })
        .and_then(|(keyed_txid, transfer)| {
            let txid = if transfer.txid.is_empty() {
                keyed_txid.clone()
            } else {
                transfer.txid.clone()
            };
            if txid.is_empty() {
                None
            } else {
                Some(TxHash(txid))
            }
        })
}

/// Maximum number of consecutive unrecognized `check_tx_key` failures before the
/// watch gives up. A lock transaction that vanished from the chain (deep reorg)
/// otherwise loops forever with every error treated as transient.
const MAX_CONSECUTIVE_UNKNOWN_ERRORS: u32 = 30;

#[cfg(test)]
async fn wait_for_confirmations<C, H>(
    client: &Mutex<C>,
    transfer_proof: TransferProof,
    to_address: Address,
    expected: Amount,
    conf_target: u64,
    check_interval: Interval,
    wallet_name: String,
) -> Result<(), InsufficientFunds>
where
    C: wallet::MoneroWalletRpc<H> + Sync,
    H: Send + Sync + 'static + monero_rpc::jsonrpc::SendRequest,
    <H as monero_rpc::jsonrpc::SendRequest>::Error: std::error::Error + Send + Sync + 'static,
    monero_rpc::jsonrpc::Error<<H as monero_rpc::jsonrpc::SendRequest>::Error>:
        From<<H as monero_rpc::jsonrpc::SendRequest>::Error>,
{
    wait_for_confirmations_with_progress::<C, H, fn(&str, u64)>(
        client,
        transfer_proof,
        to_address,
        expected,
        conf_target,
        check_interval,
        wallet_name,
        None,
    )
    .await
}

async fn wait_for_confirmations_with_progress<C, H, F>(
    client: &Mutex<C>,
    transfer_proof: TransferProof,
    to_address: Address,
    expected: Amount,
    conf_target: u64,
    mut check_interval: Interval,
    wallet_name: String,
    on_confirmation: Option<F>,
) -> Result<(), InsufficientFunds>
where
    C: wallet::MoneroWalletRpc<H> + Sync,
    H: Send + Sync + 'static + monero_rpc::jsonrpc::SendRequest,
    <H as monero_rpc::jsonrpc::SendRequest>::Error: std::error::Error + Send + Sync + 'static,
    monero_rpc::jsonrpc::Error<<H as monero_rpc::jsonrpc::SendRequest>::Error>:
        From<<H as monero_rpc::jsonrpc::SendRequest>::Error>,
    F: Fn(&str, u64),
{
    let mut seen_confirmations = 0u64;
    let mut consecutive_unknown_errors = 0u32;

    while seen_confirmations < conf_target {
        check_interval.tick().await; // tick() at the beginning of the loop so every `continue` tick()s as well

        let txid = transfer_proof.tx_hash().to_string();
        let client = client.lock().await;

        let tx = match client
            .check_tx_key(
                txid.clone(),
                transfer_proof.tx_key.to_string(),
                to_address.to_string(),
            )
            .await
        {
            Ok(proof) => {
                consecutive_unknown_errors = 0;
                proof
            }
            Err(jsonrpc::Error::JsonRpc(jsonrpc::JsonRpcError {
                code: -1,
                message,
                data,
            })) => {
                tracing::debug!(message, ?data);
                tracing::warn!(%txid, message, "`monero-wallet-rpc` failed to fetch transaction, may need to be restarted");
                continue;
            }
            // TODO: Implement this using a generic proxy for each function call once https://github.com/thomaseizinger/rust-jsonrpc-client/issues/47 is fixed.
            Err(jsonrpc::Error::JsonRpc(jsonrpc::JsonRpcError { code: -13, .. })) => {
                tracing::debug!(
                    "Opening wallet `{}` because no wallet is loaded",
                    wallet_name
                );
                let _ = client.open_wallet(wallet_name.clone()).await;
                continue;
            }
            Err(other) => {
                consecutive_unknown_errors += 1;
                if consecutive_unknown_errors >= MAX_CONSECUTIVE_UNKNOWN_ERRORS {
                    tracing::error!(
                        %txid,
                        consecutive_unknown_errors,
                        "Failed to retrieve the Monero lock tx too many times in a row; it may have vanished from the chain (deep reorg). Failing the watch so the swap can be recovered"
                    );
                    return Err(InsufficientFunds {
                        expected,
                        actual: Amount::from_piconero(0),
                    });
                }
                tracing::debug!(
                    %txid,
                    "Failed to retrieve tx from blockchain: {:#}", other
                );
                continue; // treating every error as transient and retrying
                          // is obviously wrong but the jsonrpc client is
                          // too primitive to differentiate between all the
                          // cases
            }
        };

        let received = Amount::from_piconero(tx.received);

        if received != expected {
            return Err(InsufficientFunds {
                expected,
                actual: received,
            });
        }

        if tx.confirmations >= conf_target {
            // Re-verify the lock transaction before treating it as final: a double-spend
            // (e.g. surfaced by a reorg) means the XMR lock may no longer exist, and
            // proceeding would hand the encrypted signature to Alice with nothing locked.
            match client.get_transfer_by_txid(txid.clone()).await {
                Ok(transfer) if transfer.transfer.double_spend_seen => {
                    tracing::error!(%txid, "Monero lock tx is double-spent; refusing to treat it as final");
                    return Err(InsufficientFunds {
                        expected,
                        actual: Amount::from_piconero(0),
                    });
                }
                Ok(_) => {}
                Err(jsonrpc::Error::JsonRpc(jsonrpc::JsonRpcError { code: -8, .. })) => {
                    // -8 "Transaction not found" is the expected steady state
                    // for Bob: his wallet is blind to Alice's lock transaction
                    // by design (it only holds its own key material, not the
                    // aggregate view key), so the transfer can never appear in
                    // his wallet's transfer list. That outcome carries no
                    // double-spend signal, so fall through and treat the lock
                    // as final.
                }
                Err(error) => {
                    // Other lookup failures (e.g. no wallet loaded yet) are
                    // genuinely transient; re-verify on a later tick.
                    tracing::debug!(
                        %txid,
                        "Transfer lookup for the double-spend check failed: {:#}", error
                    );
                    continue;
                }
            }
        }

        // Report the latest observation, including a decrease after a reorg.
        // Never synthesize counts skipped between wallet-RPC polls.
        if tx.confirmations != seen_confirmations {
            seen_confirmations = tx.confirmations;
            if let Some(on_confirmation) = on_confirmation.as_ref() {
                on_confirmation(&txid, seen_confirmations);
            }
            tracing::info!(
                %txid,
                %seen_confirmations,
                needed_confirmations = %conf_target,
                "Received new confirmation for Monero lock tx"
            );
        }
    }

    Ok(())
}

pub async fn wait_for_confs<C, H>(
    env: Option<&JNIEnv<'_>>,
    client: &Mutex<C>,
    transfer_proof: TransferProof,
    to_address: Address,
    expected: Amount,
    conf_target: u64,
    mut check_interval: Interval,
    wallet_name: String,
) -> Result<(), InsufficientFunds>
where
    C: wallet::MoneroWalletRpc<H> + Sync,
    H: Send + Sync + 'static + monero_rpc::jsonrpc::SendRequest,
    <H as monero_rpc::jsonrpc::SendRequest>::Error: std::error::Error + Send + Sync + 'static,
    monero_rpc::jsonrpc::Error<<H as monero_rpc::jsonrpc::SendRequest>::Error>:
        From<<H as monero_rpc::jsonrpc::SendRequest>::Error>,
{
    let mut seen_confirmations = 0u64;
    let mut consecutive_unknown_errors = 0u32;

    while seen_confirmations < conf_target {
        check_interval.tick().await; // tick() at the beginning of the loop so every `continue` tick()s as well

        let txid = transfer_proof.tx_hash().to_string();
        let client = client.lock().await;

        let tx = match client
            .check_tx_key(
                txid.clone(),
                transfer_proof.tx_key.to_string(),
                to_address.to_string(),
            )
            .await
        {
            Ok(proof) => {
                consecutive_unknown_errors = 0;
                proof
            }
            Err(jsonrpc::Error::JsonRpc(jsonrpc::JsonRpcError {
                code: -1,
                message,
                data,
            })) => {
                tracing::debug!(message, ?data);
                tracing::warn!(%txid, message, "`monero-wallet-rpc` failed to fetch transaction, may need to be restarted");
                continue;
            }
            // TODO: Implement this using a generic proxy for each function call once https://github.com/thomaseizinger/rust-jsonrpc-client/issues/47 is fixed.
            Err(jsonrpc::Error::JsonRpc(jsonrpc::JsonRpcError { code: -13, .. })) => {
                tracing::debug!(
                    "Opening wallet `{}` because no wallet is loaded",
                    wallet_name
                );
                let _ = client.open_wallet(wallet_name.clone()).await;
                continue;
            }
            Err(other) => {
                consecutive_unknown_errors += 1;
                if consecutive_unknown_errors >= MAX_CONSECUTIVE_UNKNOWN_ERRORS {
                    tracing::error!(
                        %txid,
                        consecutive_unknown_errors,
                        "Failed to retrieve the Monero lock tx too many times in a row; it may have vanished from the chain (deep reorg). Failing the watch so the swap can be recovered"
                    );
                    return Err(InsufficientFunds {
                        expected,
                        actual: Amount::from_piconero(0),
                    });
                }
                tracing::debug!(
                    %txid,
                    "Failed to retrieve tx from blockchain: {:#}", other
                );
                continue; // treating every error as transient and retrying
                          // is obviously wrong but the jsonrpc client is
                          // too primitive to differentiate between all the
                          // cases
            }
        };

        let received = Amount::from_piconero(tx.received);

        if received != expected {
            return Err(InsufficientFunds {
                expected,
                actual: received,
            });
        }

        if tx.confirmations >= conf_target {
            // Re-verify the lock transaction before treating it as final: a double-spend
            // (e.g. surfaced by a reorg) means the XMR lock may no longer exist, and
            // proceeding would hand the encrypted signature to Alice with nothing locked.
            match client.get_transfer_by_txid(txid.clone()).await {
                Ok(transfer) if transfer.transfer.double_spend_seen => {
                    tracing::error!(%txid, "Monero lock tx is double-spent; refusing to treat it as final");
                    return Err(InsufficientFunds {
                        expected,
                        actual: Amount::from_piconero(0),
                    });
                }
                Ok(_) => {}
                Err(jsonrpc::Error::JsonRpc(jsonrpc::JsonRpcError { code: -8, .. })) => {
                    // -8 "Transaction not found" is the expected steady state
                    // for Bob: his wallet is blind to Alice's lock transaction
                    // by design (it only holds its own key material, not the
                    // aggregate view key), so the transfer can never appear in
                    // his wallet's transfer list. That outcome carries no
                    // double-spend signal, so fall through and treat the lock
                    // as final.
                }
                Err(error) => {
                    // Other lookup failures (e.g. no wallet loaded yet) are
                    // genuinely transient; re-verify on a later tick.
                    tracing::debug!(
                        %txid,
                        "Transfer lookup for the double-spend check failed: {:#}", error
                    );
                    continue;
                }
            }
        }

        if tx.confirmations > seen_confirmations {
            seen_confirmations = tx.confirmations;
            if env.is_some() {
                util::on_xmr_lock_confirmation(
                    env.unwrap(),
                    txid.clone(),
                    seen_confirmations.clone(),
                );
            }
            tracing::info!(
                %txid,
                %seen_confirmations,
                needed_confirmations = %conf_target,
                "Received new confirmation for Monero lock tx"
            );
        }
    }

    Ok(())
}

/// Shared implementation of [`Wallet::create_from`], generic over the wallet-rpc
/// client so it can be exercised with a scripted client in tests.
///
/// Generates (or re-opens) the swap's temporary refund wallet, refreshes it and
/// sweeps its entire balance back to the ASB's main wallet, then re-opens the
/// main wallet. Every failure is propagated to the caller: the swap must stay
/// in its non-terminal `BtcRefunded` state (which is resumed and retried) until
/// the funds have actually landed in the main wallet.
///
/// The sweep is idempotent across retries and restarts: if the wallet holds no
/// funds after a successful refresh, a previous sweep already broadcast (its
/// outputs leave the balance immediately) or there is nothing to sweep, so the
/// operation succeeds without issuing a second spend.
async fn create_from_and_sweep<C, H>(
    client: &Mutex<C>,
    network: Network,
    main_wallet_name: &str,
    main_address: &str,
    file_name: String,
    private_spend_key: PrivateKey,
    private_view_key: PrivateViewKey,
    restore_height: BlockHeight,
) -> Result<()>
where
    C: wallet::MoneroWalletRpc<H> + Send + Sync,
    H: Send + Sync + 'static + monero_rpc::jsonrpc::SendRequest,
    <H as monero_rpc::jsonrpc::SendRequest>::Error: std::error::Error + Send + Sync + 'static,
    monero_rpc::jsonrpc::Error<<H as monero_rpc::jsonrpc::SendRequest>::Error>:
        From<<H as monero_rpc::jsonrpc::SendRequest>::Error>,
{
    let public_spend_key = PublicKey::from_private_key(&private_spend_key);
    let public_view_key = PublicKey::from_private_key(&private_view_key.into());

    let temp_wallet_address = Address::standard(network, public_spend_key, public_view_key);

    let wallet = client.lock().await;

    // Close the main wallet before generating the other wallet to ensure that
    // it saves its state correctly
    let _ = wallet.store().await?;
    let _ = wallet.close_wallet().await?;

    if let Err(e) = wallet
        .generate_from_keys(
            file_name.clone(),
            temp_wallet_address.to_string(),
            private_spend_key.to_string(),
            PrivateKey::from(private_view_key).to_string(),
            restore_height.height,
            String::from(""),
            true,
        )
        .await
    {
        // When resuming after a failure the wallet might already exist; open it
        // so the sweep below can still run (or verify the balance is spent).
        tracing::warn!("Failed to generate monero wallet from keys: {:#}", e);
        tracing::info!(
            %file_name,
            "Falling back to trying to open the the wallet if it already exists",
        );
        wallet.open_wallet(file_name.clone()).await?;
    }

    let sweep_result: Result<()> = async {
        wallet
            .refresh()
            .await
            .context("Failed to refresh the refund wallet")?;

        let balance = wallet
            .get_balance(0)
            .await
            .context("Failed to read the refund wallet balance")?;

        if balance.balance == 0 {
            tracing::info!(
                wallet = %file_name,
                "Refund wallet holds no funds after refresh; the sweep was already completed (or there is nothing to sweep)"
            );
            return Ok(());
        }

        let sweep_all = wallet
            .sweep_all(main_address.to_string(), false, false, false, false)
            .await
            .context("Failed to sweep the refund wallet back to the main wallet")?;

        for tx in sweep_all.tx_hash_list {
            tracing::info!(
                %tx,
                monero_address = %main_address,
                "ASB_REFUND_SWEEP_TX"
            );
        }

        Ok(())
    }
    .await;

    // Always return to the main wallet, even after a failed refresh/sweep, so
    // the ASB keeps operating on the wallet it expects.
    if let Err(error) = wallet.open_wallet(main_wallet_name.to_owned()).await {
        tracing::warn!(
            wallet = %main_wallet_name,
            "Failed to re-open the main wallet after the refund sweep: {:#}", error
        );
    }

    sweep_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracing_ext::capture_logs;
    use monero_rpc::wallet::CheckTxKey;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tracing::metadata::LevelFilter;

    async fn confirmation_progress(
        responses: Vec<Result<CheckTxKey, (ErrorCode, ErrorMessage)>>,
        transfer_responses: Vec<Result<bool, i64>>,
    ) -> (Result<(), InsufficientFunds>, Vec<(String, u64)>, u32, u32) {
        let client = Mutex::new(DummyClient::with_transfer_responses(responses, transfer_responses));
        let progress = RefCell::new(Vec::new());
        let result = wait_for_confirmations_with_progress(
            &client,
            TransferProof::new(TxHash("lock-tx".to_owned()), PrivateKey {
                scalar: crate::monero::Scalar::random(&mut rand::thread_rng()),
            }),
            "53H3QthYLckeCXh9u38vohb2gZ4QgEG3FMWHNxccR6MqV1LdDVYwF1FKsRJPj4tTupWLf9JtGPBcn2MVN6c9oR7p5Uf7JdJ".parse().unwrap(),
            Amount::from_piconero(100),
            10,
            tokio::time::interval(Duration::from_millis(1)),
            "foo-wallet".to_owned(),
            Some(|txid: &str, count| progress.borrow_mut().push((txid.to_owned(), count))),
        ).await;
        let client = client.lock().await;
        let checks = client.check_tx_key_invocations.load(Ordering::SeqCst);
        let lookups = client.get_transfer_by_txid_invocations.load(Ordering::SeqCst);
        (result, progress.into_inner(), checks, lookups)
    }

    fn proof(confirmations: u64, received: u64) -> Result<CheckTxKey, (ErrorCode, ErrorMessage)> {
        Ok(CheckTxKey { confirmations, received })
    }

    #[tokio::test]
    async fn confirmation_progress_reports_only_observed_changes_and_finality() {
        let (result, progress, checks, lookups) = confirmation_progress(
            vec![proof(0, 100), proof(1, 100), proof(1, 100), proof(5, 100), proof(3, 100), proof(0, 100), proof(7, 100), proof(10, 100)],
            vec![Ok(false)],
        ).await;
        assert!(result.is_ok());
        assert_eq!(progress, vec![("lock-tx".to_owned(), 1), ("lock-tx".to_owned(), 5), ("lock-tx".to_owned(), 3), ("lock-tx".to_owned(), 0), ("lock-tx".to_owned(), 7), ("lock-tx".to_owned(), 10)]);
        assert_eq!((checks, lookups), (8, 1));
    }

    #[tokio::test]
    async fn confirmation_progress_resume_reports_actual_count_above_target() {
        let (result, progress, checks, lookups) = confirmation_progress(
            vec![proof(14, 100)], vec![Err(-8)],
        ).await;
        assert!(result.is_ok());
        assert_eq!(progress, vec![("lock-tx".to_owned(), 14)]);
        assert_eq!((checks, lookups), (1, 1));
    }

    #[tokio::test]
    async fn confirmation_progress_resume_reports_exact_target_after_recheck() {
        let (result, progress, checks, lookups) = confirmation_progress(
            vec![proof(10, 100)], vec![Ok(false)],
        ).await;
        assert!(result.is_ok());
        assert_eq!(progress, vec![("lock-tx".to_owned(), 10)]);
        assert_eq!((checks, lookups), (1, 1));
    }

    #[tokio::test]
    async fn confirmation_progress_waits_for_successful_finality_recheck() {
        let (result, progress, checks, lookups) = confirmation_progress(
            vec![proof(9, 100), proof(10, 100), proof(8, 100), proof(10, 100)],
            vec![Err(-32000), Ok(false)],
        ).await;
        assert!(result.is_ok());
        assert_eq!(progress, vec![("lock-tx".to_owned(), 9), ("lock-tx".to_owned(), 8), ("lock-tx".to_owned(), 10)]);
        assert_eq!((checks, lookups), (4, 2));
    }

    #[tokio::test]
    async fn confirmation_progress_does_not_report_double_spent_or_wrong_amount() {
        let (result, progress, _, lookups) = confirmation_progress(
            vec![proof(2, 100), proof(10, 100)], vec![Ok(true)],
        ).await;
        assert!(result.is_err());
        assert_eq!(progress, vec![("lock-tx".to_owned(), 2)]);
        assert_eq!(lookups, 1);

        let (result, progress, _, lookups) = confirmation_progress(
            vec![proof(2, 100), proof(10, 99)], vec![],
        ).await;
        assert!(result.is_err());
        assert_eq!(progress, vec![("lock-tx".to_owned(), 2)]);
        assert_eq!(lookups, 0);
    }

    #[tokio::test]
    async fn confirmation_progress_retries_rpc_errors_without_inventing_events() {
        let (result, progress, checks, lookups) = confirmation_progress(
            vec![proof(4, 100), Err((-13, "No wallet file".to_owned())), Err((-1, "unavailable".to_owned())), proof(4, 100), proof(10, 100)],
            vec![Ok(false)],
        ).await;
        assert!(result.is_ok());
        assert_eq!(progress, vec![("lock-tx".to_owned(), 4), ("lock-tx".to_owned(), 10)]);
        assert_eq!((checks, lookups), (5, 1));
    }

    #[tokio::test]
    async fn confirmation_progress_stops_after_unknown_rpc_errors() {
        let mut responses = vec![proof(4, 100)];
        responses.extend(vec![Err((-32000, "Transaction not found".to_owned())); MAX_CONSECUTIVE_UNKNOWN_ERRORS as usize]);
        let (result, progress, checks, lookups) = confirmation_progress(responses, vec![]).await;
        assert!(result.is_err());
        assert_eq!(progress, vec![("lock-tx".to_owned(), 4)]);
        assert_eq!(checks, MAX_CONSECUTIVE_UNKNOWN_ERRORS + 1);
        assert_eq!(lookups, 0);
    }

    #[tokio::test]
    async fn given_exact_confirmations_does_not_fetch_tx_again() {
        let client = Mutex::new(DummyClient::new(vec![Ok(CheckTxKey {
            confirmations: 10,
            received: 100,
        })]));

        let result = wait_for_confirmations(
            &client,
            TransferProof::new(TxHash("<FOO>".to_owned()), PrivateKey {
                scalar: crate::monero::Scalar::random(&mut rand::thread_rng())
            }),
            "53H3QthYLckeCXh9u38vohb2gZ4QgEG3FMWHNxccR6MqV1LdDVYwF1FKsRJPj4tTupWLf9JtGPBcn2MVN6c9oR7p5Uf7JdJ".parse().unwrap(),
            Amount::from_piconero(100),
            10,
            tokio::time::interval(Duration::from_millis(10)),
            "foo-wallet".to_owned()
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(
            client
                .lock()
                .await
                .check_tx_key_invocations
                .load(Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn visual_log_check() {
        let writer = capture_logs(LevelFilter::INFO);

        let client = Mutex::new(DummyClient::new(vec![
            Ok(CheckTxKey {
                confirmations: 1,
                received: 100,
            }),
            Ok(CheckTxKey {
                confirmations: 1,
                received: 100,
            }),
            Ok(CheckTxKey {
                confirmations: 1,
                received: 100,
            }),
            Ok(CheckTxKey {
                confirmations: 3,
                received: 100,
            }),
            Ok(CheckTxKey {
                confirmations: 5,
                received: 100,
            }),
        ]));

        wait_for_confirmations(
            &client,
            TransferProof::new(TxHash("<FOO>".to_owned()), PrivateKey {
                scalar: crate::monero::Scalar::random(&mut rand::thread_rng())
            }),
            "53H3QthYLckeCXh9u38vohb2gZ4QgEG3FMWHNxccR6MqV1LdDVYwF1FKsRJPj4tTupWLf9JtGPBcn2MVN6c9oR7p5Uf7JdJ".parse().unwrap(),
            Amount::from_piconero(100),
            5,
            tokio::time::interval(Duration::from_millis(10)),
            "foo-wallet".to_owned()
        )
        .await
        .unwrap();

        assert_eq!(
            writer.captured(),
            r" INFO swap::monero::wallet: Received new confirmation for Monero lock tx txid=<FOO> seen_confirmations=1 needed_confirmations=5
 INFO swap::monero::wallet: Received new confirmation for Monero lock tx txid=<FOO> seen_confirmations=3 needed_confirmations=5
 INFO swap::monero::wallet: Received new confirmation for Monero lock tx txid=<FOO> seen_confirmations=5 needed_confirmations=5
"
        );
    }

    #[tokio::test]
    async fn reopens_wallet_in_case_not_available() {
        let writer = capture_logs(LevelFilter::DEBUG);

        let client = Mutex::new(DummyClient::new(vec![
            Ok(CheckTxKey {
                confirmations: 1,
                received: 100,
            }),
            Ok(CheckTxKey {
                confirmations: 1,
                received: 100,
            }),
            Err((-13, "No wallet file".to_owned())),
            Ok(CheckTxKey {
                confirmations: 3,
                received: 100,
            }),
            Ok(CheckTxKey {
                confirmations: 5,
                received: 100,
            }),
        ]));

        wait_for_confirmations(
            &client,
            TransferProof::new(TxHash("<FOO>".to_owned()), PrivateKey {
                scalar: crate::monero::Scalar::random(&mut rand::thread_rng())
            }),
            "53H3QthYLckeCXh9u38vohb2gZ4QgEG3FMWHNxccR6MqV1LdDVYwF1FKsRJPj4tTupWLf9JtGPBcn2MVN6c9oR7p5Uf7JdJ".parse().unwrap(),
            Amount::from_piconero(100),
            5,
            tokio::time::interval(Duration::from_millis(10)),
            "foo-wallet".to_owned()
        )
        .await
        .unwrap();

        assert_eq!(
            writer.captured(),
            r" INFO swap::monero::wallet: Received new confirmation for Monero lock tx txid=<FOO> seen_confirmations=1 needed_confirmations=5
DEBUG swap::monero::wallet: Opening wallet `foo-wallet` because no wallet is loaded
 INFO swap::monero::wallet: Received new confirmation for Monero lock tx txid=<FOO> seen_confirmations=3 needed_confirmations=5
 INFO swap::monero::wallet: Received new confirmation for Monero lock tx txid=<FOO> seen_confirmations=5 needed_confirmations=5
"
        );
        assert_eq!(
            client
                .lock()
                .await
                .open_wallet_invocations
                .load(Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn double_spent_lock_tx_fails_the_watch() {
        let client = Mutex::new(DummyClient::with_transfer_responses(
            vec![Ok(CheckTxKey {
                confirmations: 10,
                received: 100,
            })],
            vec![Ok(true)],
        ));

        let result = wait_for_confirmations(
            &client,
            TransferProof::new(TxHash("<FOO>".to_owned()), PrivateKey {
                scalar: crate::monero::Scalar::random(&mut rand::thread_rng())
            }),
            "53H3QthYLckeCXh9u38vohb2gZ4QgEG3FMWHNxccR6MqV1LdDVYwF1FKsRJPj4tTupWLf9JtGPBcn2MVN6c9oR7p5Uf7JdJ".parse().unwrap(),
            Amount::from_piconero(100),
            10,
            tokio::time::interval(Duration::from_millis(10)),
            "foo-wallet".to_owned()
        )
        .await;

        assert!(
            result.is_err(),
            "a double-spent lock tx must not be treated as final"
        );
        assert_eq!(
            client
                .lock()
                .await
                .check_tx_key_invocations
                .load(Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn transient_transfer_lookup_failure_retries_the_gate() {
        let client = Mutex::new(DummyClient::with_transfer_responses(
            vec![
                Ok(CheckTxKey {
                    confirmations: 10,
                    received: 100,
                }),
                Ok(CheckTxKey {
                    confirmations: 10,
                    received: 100,
                }),
            ],
            vec![Err(-32000), Ok(false)],
        ));

        wait_for_confirmations(
            &client,
            TransferProof::new(TxHash("<FOO>".to_owned()), PrivateKey {
                scalar: crate::monero::Scalar::random(&mut rand::thread_rng())
            }),
            "53H3QthYLckeCXh9u38vohb2gZ4QgEG3FMWHNxccR6MqV1LdDVYwF1FKsRJPj4tTupWLf9JtGPBcn2MVN6c9oR7p5Uf7JdJ".parse().unwrap(),
            Amount::from_piconero(100),
            10,
            tokio::time::interval(Duration::from_millis(10)),
            "foo-wallet".to_owned()
        )
        .await
        .unwrap();

        assert_eq!(
            client
                .lock()
                .await
                .check_tx_key_invocations
                .load(Ordering::SeqCst),
            2
        );
    }

    #[tokio::test]
    async fn persistently_unknown_tx_fails_the_watch() {
        let client = Mutex::new(DummyClient::new(vec![
            Err((
                -32000,
                "Transaction not found".to_owned()
            ));
            MAX_CONSECUTIVE_UNKNOWN_ERRORS
                as usize
        ]));

        let result = wait_for_confirmations(
            &client,
            TransferProof::new(TxHash("<FOO>".to_owned()), PrivateKey {
                scalar: crate::monero::Scalar::random(&mut rand::thread_rng())
            }),
            "53H3QthYLckeCXh9u38vohb2gZ4QgEG3FMWHNxccR6MqV1LdDVYwF1FKsRJPj4tTupWLf9JtGPBcn2MVN6c9oR7p5Uf7JdJ".parse().unwrap(),
            Amount::from_piconero(100),
            10,
            tokio::time::interval(Duration::from_millis(10)),
            "foo-wallet".to_owned()
        )
        .await;

        assert!(
            result.is_err(),
            "a vanished lock tx must fail the watch instead of looping forever"
        );
    }

    type ErrorCode = i64;
    type ErrorMessage = String;

    struct DummyClient {
        check_tx_key_responses: Vec<Result<wallet::CheckTxKey, (ErrorCode, ErrorMessage)>>,
        get_transfer_by_txid_responses: Vec<Result<bool, i64>>,
        get_transfer_by_txid_confirmations: Vec<u64>,
        refresh_results: Vec<Result<wallet::Refreshed, ()>>,
        sweep_all_results: Vec<Result<Vec<String>, ()>>,
        get_balance_results: Vec<Result<wallet::GetBalance, ()>>,
        refresh_sequence: AtomicU32,
        height_sequence: AtomicU32,
        call_sequence: AtomicU32,

        check_tx_key_invocations: AtomicU32,
        get_transfer_by_txid_invocations: AtomicU32,
        open_wallet_invocations: AtomicU32,
        refresh_invocations: AtomicU32,
        sweep_all_invocations: AtomicU32,
        get_balance_invocations: AtomicU32,
        store_invocations: AtomicU32,
        close_wallet_invocations: AtomicU32,
        generate_from_keys_invocations: AtomicU32,
    }

    impl DummyClient {
        fn new(
            check_tx_key_responses: Vec<Result<wallet::CheckTxKey, (ErrorCode, ErrorMessage)>>,
        ) -> Self {
            Self::with_transfer_responses(check_tx_key_responses, vec![])
        }

        fn with_transfer_responses(
            check_tx_key_responses: Vec<Result<wallet::CheckTxKey, (ErrorCode, ErrorMessage)>>,
            get_transfer_by_txid_responses: Vec<Result<bool, i64>>,
        ) -> Self {
            Self {
                check_tx_key_responses,
                get_transfer_by_txid_responses,
                get_transfer_by_txid_confirmations: vec![],
                refresh_results: vec![],
                sweep_all_results: vec![],
                get_balance_results: vec![],
                refresh_sequence: Default::default(),
                height_sequence: Default::default(),
                call_sequence: Default::default(),
                check_tx_key_invocations: Default::default(),
                get_transfer_by_txid_invocations: Default::default(),
                open_wallet_invocations: Default::default(),
                refresh_invocations: Default::default(),
                sweep_all_invocations: Default::default(),
                get_balance_invocations: Default::default(),
                store_invocations: Default::default(),
                close_wallet_invocations: Default::default(),
                generate_from_keys_invocations: Default::default(),
            }
        }

        fn for_sweep(
            refresh_results: Vec<Result<wallet::Refreshed, ()>>,
            get_balance_results: Vec<Result<wallet::GetBalance, ()>>,
            sweep_all_results: Vec<Result<Vec<String>, ()>>,
        ) -> Self {
            Self {
                check_tx_key_responses: vec![],
                get_transfer_by_txid_responses: vec![],
                get_transfer_by_txid_confirmations: vec![],
                refresh_results,
                sweep_all_results,
                get_balance_results,
                refresh_sequence: Default::default(),
                height_sequence: Default::default(),
                call_sequence: Default::default(),
                check_tx_key_invocations: Default::default(),
                get_transfer_by_txid_invocations: Default::default(),
                open_wallet_invocations: Default::default(),
                refresh_invocations: Default::default(),
                sweep_all_invocations: Default::default(),
                get_balance_invocations: Default::default(),
                store_invocations: Default::default(),
                close_wallet_invocations: Default::default(),
                generate_from_keys_invocations: Default::default(),
            }
        }
    }

    #[async_trait::async_trait]
    impl monero_rpc::wallet::MoneroWalletRpc<reqwest::Client> for DummyClient {
        async fn open_wallet(
            &self,
            _: String,
        ) -> Result<wallet::WalletOpened, monero_rpc::jsonrpc::Error<reqwest::Error>> {
            self.open_wallet_invocations.fetch_add(1, Ordering::SeqCst);

            Ok(monero_rpc::wallet::Empty {})
        }

        async fn check_tx_key(
            &self,
            _: String,
            _: String,
            _: String,
        ) -> Result<wallet::CheckTxKey, monero_rpc::jsonrpc::Error<reqwest::Error>> {
            let index = self.check_tx_key_invocations.fetch_add(1, Ordering::SeqCst);

            self.check_tx_key_responses[index as usize]
                .clone()
                .map_err(|(code, message)| {
                    monero_rpc::jsonrpc::Error::JsonRpc(monero_rpc::jsonrpc::JsonRpcError {
                        code,
                        message,
                        data: None,
                    })
                })
        }

        async fn get_transfer_by_txid(
            &self,
            _: String,
        ) -> Result<wallet::GetTransferByTxid, monero_rpc::jsonrpc::Error<reqwest::Error>> {
            let index = self
                .get_transfer_by_txid_invocations
                .fetch_add(1, Ordering::SeqCst);

            // Unscripted lookups report no double-spend; scripted entries drive the
            // double-spend gate (bool) or a transient lookup failure (Err).
            let double_spend_seen = match self.get_transfer_by_txid_responses.get(index as usize) {
                Some(Ok(double_spend_seen)) => *double_spend_seen,
                Some(Err(code)) => {
                    return Err(monero_rpc::jsonrpc::Error::JsonRpc(
                        monero_rpc::jsonrpc::JsonRpcError {
                            code: *code,
                            message: "transfer not found".to_owned(),
                            data: None,
                        },
                    ));
                }
                None => false,
            };

            Ok(wallet::GetTransferByTxid {
                transfer: wallet::TransferInfo {
                    double_spend_seen,
                    txid: String::new(),
                    amount: 0,
                    address: String::new(),
                    destinations: Vec::new(),
                    confirmations: self
                        .get_transfer_by_txid_confirmations
                        .get(index as usize)
                        .copied()
                        .unwrap_or(0),
                    tx_key: None,
                },
            })
        }

        async fn send_request<P>(
            &self,
            _: String,
        ) -> Result<monero_rpc::jsonrpc::Response<P>, reqwest::Error>
        where
            P: serde::de::DeserializeOwned,
        {
            todo!()
        }

        async fn refresh(
            &self,
        ) -> Result<wallet::Refreshed, monero_rpc::jsonrpc::Error<reqwest::Error>> {
            let index = self.refresh_invocations.fetch_add(1, Ordering::SeqCst);
            let sequence = self.call_sequence.fetch_add(1, Ordering::SeqCst) + 1;
            self.refresh_sequence.store(sequence, Ordering::SeqCst);

            match self.refresh_results.get(index as usize) {
                Some(Ok(refreshed)) => Ok(*refreshed),
                Some(Err(())) => Err(jsonrpc_error()),
                None => Ok(wallet::Refreshed {
                    blocks_fetched: 0,
                    received_money: false,
                }),
            }
        }

        async fn get_height(
            &self,
        ) -> Result<wallet::BlockHeight, monero_rpc::jsonrpc::Error<reqwest::Error>> {
            let sequence = self.call_sequence.fetch_add(1, Ordering::SeqCst) + 1;
            self.height_sequence.store(sequence, Ordering::SeqCst);
            Ok(wallet::BlockHeight { height: 123 })
        }

        async fn sweep_all(
            &self,
            _: String,
            _: bool,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<wallet::SweepAll, monero_rpc::jsonrpc::Error<reqwest::Error>> {
            let index = self.sweep_all_invocations.fetch_add(1, Ordering::SeqCst);

            match self.sweep_all_results.get(index as usize) {
                Some(Ok(tx_hash_list)) => Ok(wallet::SweepAll {
                    tx_hash_list: tx_hash_list.clone(),
                    tx_key_list: vec![],
                    tx_blob_list: vec![],
                    tx_metadata_list: vec![],
                    fee_list: vec![],
                }),
                Some(Err(())) => Err(jsonrpc_error()),
                None => Ok(wallet::SweepAll {
                    tx_hash_list: vec![],
                    tx_key_list: vec![],
                    tx_blob_list: vec![],
                    tx_metadata_list: vec![],
                    fee_list: vec![],
                }),
            }
        }

        async fn get_balance(
            &self,
            _: u32,
        ) -> Result<wallet::GetBalance, monero_rpc::jsonrpc::Error<reqwest::Error>> {
            let index = self.get_balance_invocations.fetch_add(1, Ordering::SeqCst);

            match self.get_balance_results.get(index as usize) {
                Some(Ok(balance)) => Ok(*balance),
                Some(Err(())) => Err(jsonrpc_error()),
                None => Ok(wallet::GetBalance {
                    balance: 0,
                    unlocked_balance: 0,
                    multisig_import_needed: false,
                    blocks_to_unlock: 0,
                    time_to_unlock: 0,
                }),
            }
        }

        async fn store(&self) -> Result<(), monero_rpc::jsonrpc::Error<reqwest::Error>> {
            self.store_invocations.fetch_add(1, Ordering::SeqCst);

            Ok(())
        }

        async fn close_wallet(
            &self,
        ) -> Result<wallet::Empty, monero_rpc::jsonrpc::Error<reqwest::Error>> {
            self.close_wallet_invocations.fetch_add(1, Ordering::SeqCst);

            Ok(wallet::Empty {})
        }

        async fn generate_from_keys(
            &self,
            _: String,
            _: String,
            _: String,
            _: String,
            _: u32,
            _: String,
            _: bool,
        ) -> Result<wallet::GenerateFromKeys, monero_rpc::jsonrpc::Error<reqwest::Error>> {
            self.generate_from_keys_invocations
                .fetch_add(1, Ordering::SeqCst);

            Ok(wallet::GenerateFromKeys {
                address: String::new(),
                info: String::new(),
            })
        }
    }

    fn jsonrpc_error() -> monero_rpc::jsonrpc::Error<reqwest::Error> {
        monero_rpc::jsonrpc::Error::JsonRpc(monero_rpc::jsonrpc::JsonRpcError {
            code: -1,
            message: "scripted failure".to_owned(),
            data: None,
        })
    }

    #[derive(Default)]
    struct FakeRestoreHeightWallet {
        sequence: AtomicU32,
        refresh_sequence: AtomicU32,
        height_sequence: AtomicU32,
    }

    #[async_trait::async_trait]
    impl RestoreHeightWallet for FakeRestoreHeightWallet {
        async fn refresh_for_restore_height(&self) -> Result<()> {
            let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
            self.refresh_sequence.store(sequence, Ordering::SeqCst);
            Ok(())
        }

        async fn block_height_for_restore_height(&self) -> Result<BlockHeight> {
            let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
            self.height_sequence.store(sequence, Ordering::SeqCst);
            Ok(BlockHeight { height: 123 })
        }
    }

    #[tokio::test]
    async fn transient_wallet_error_is_retried_until_it_succeeds() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let calls = Arc::new(AtomicU32::new(0));
        let calls_for_op = Arc::clone(&calls);
        let result = retry_with_backoff(move || {
            let calls = Arc::clone(&calls_for_op);
            async move {
                let attempt = calls.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    Err(anyhow::anyhow!("JSON-RPC -38: no connection to daemon"))
                } else {
                    Ok(42u32)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    /// A wallet whose refresh fails with "no connection to daemon" the first `failures` times.
    struct FlakyRefreshWallet {
        failures: u32,
        refreshes: std::sync::atomic::AtomicU32,
    }

    #[async_trait::async_trait]
    impl RestoreHeightWallet for FlakyRefreshWallet {
        async fn refresh_for_restore_height(&self) -> Result<()> {
            let call = self.refreshes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call < self.failures {
                anyhow::bail!("JSON-RPC request failed with code -38: no connection to daemon");
            }
            Ok(())
        }

        async fn block_height_for_restore_height(&self) -> Result<BlockHeight> {
            Ok(BlockHeight { height: 2_249_933 })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_restore_height_read_survives_a_lost_daemon_connection() {
        let wallet = FlakyRefreshWallet { failures: 2, refreshes: Default::default() };
        let mut told = Vec::new();
        let height = refresh_then_block_height_retrying_on(&wallet, Duration::from_secs(300), |attempt, _, error| {
            told.push((attempt, error.to_string()));
        })
        .await
        .unwrap();

        assert_eq!(height.height, 2_249_933);
        assert_eq!(wallet.refreshes.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(told.len(), 2);
        assert_eq!(told[0].0, 1);
        assert!(told[1].1.contains("no connection to daemon"));
    }

    #[tokio::test(start_paused = true)]
    async fn the_restore_height_read_gives_up_after_its_budget_and_names_the_step() {
        let wallet = FlakyRefreshWallet { failures: u32::MAX, refreshes: Default::default() };
        let error = refresh_then_block_height_retrying_on(&wallet, Duration::from_secs(60), |_, _, _| {})
            .await
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("while reading the restore height"), "{message}");
        assert!(message.contains("no connection to daemon"), "{message}");
    }

    #[test]
    fn redeem_restore_height_uses_the_chain_tip_not_the_cached_height() {
        // WORKLOG §364: the check wallet's cached height (2_240_000) made the
        // redeem wallet scan ~5,000 blocks at sweep time.
        assert_eq!(
            redeem_restore_height(Some(2_245_000), 2_240_000),
            2_245_000 - REDEEM_RESTORE_HEIGHT_MARGIN
        );
    }

    #[test]
    fn redeem_restore_height_saturates_at_genesis() {
        assert_eq!(redeem_restore_height(Some(3), 0), 0);
    }

    #[test]
    fn redeem_restore_height_falls_back_to_the_recorded_height() {
        assert_eq!(redeem_restore_height(None, 2_240_000), 2_240_000);
    }

    #[tokio::test]
    async fn refresh_then_block_height_calls_refresh_first() {
        let wallet = FakeRestoreHeightWallet::default();

        let height = refresh_then_block_height(&wallet).await.unwrap();

        let refresh_sequence = wallet.refresh_sequence.load(Ordering::SeqCst);
        let height_sequence = wallet.height_sequence.load(Ordering::SeqCst);
        assert_eq!(height.height, 123);
        assert!(
            refresh_sequence > 0 && refresh_sequence < height_sequence,
            "get_height must run after refresh: refresh={refresh_sequence}, height={height_sequence}"
        );
    }

    #[tokio::test]
    async fn construction_throttle_books_slots_in_order() {
        let throttle = ConstructionThrottle::new(Duration::from_millis(100)).unwrap();

        let start = std::time::Instant::now();
        let first = throttle.wait_for_my_turn().await;
        let after_first = start.elapsed();
        let second = throttle.wait_for_my_turn().await;
        let after_second = start.elapsed();

        assert_eq!(first, Duration::from_millis(100));
        assert_eq!(second, Duration::from_millis(100));
        assert!(
            after_first < Duration::from_millis(50),
            "first caller must not be delayed: {after_first:?}"
        );
        assert!(
            after_second >= Duration::from_millis(100),
            "second caller must wait for its booked slot: {after_second:?}"
        );
    }

    #[tokio::test]
    async fn construction_throttle_rejects_zero_interval() {
        assert!(ConstructionThrottle::new(Duration::ZERO).is_err());
    }

    fn sweep_test_inputs() -> (PrivateKey, PrivateViewKey, BlockHeight) {
        (
            PrivateKey {
                scalar: crate::monero::Scalar::random(&mut rand::thread_rng()),
            },
            PrivateViewKey::new_random(&mut rand::thread_rng()),
            BlockHeight { height: 100 },
        )
    }

    #[tokio::test]
    async fn refund_sweep_failure_propagates_and_reopens_main_wallet() {
        let client = Mutex::new(DummyClient::for_sweep(
            vec![Ok(wallet::Refreshed {
                blocks_fetched: 10,
                received_money: true,
            })],
            vec![Ok(wallet::GetBalance {
                balance: 222_051_868_248,
                unlocked_balance: 222_051_868_248,
                multisig_import_needed: false,
                blocks_to_unlock: 0,
                time_to_unlock: 0,
            })],
            vec![Err(())],
        ));

        let (spend_key, view_key, restore_height) = sweep_test_inputs();
        let result = create_from_and_sweep(
            &client,
            Network::Stagenet,
            "asb-wallet",
            "52UNVoFakeMainAddress",
            "2a779092".to_owned(),
            spend_key,
            view_key,
            restore_height,
        )
        .await;

        assert!(
            result.is_err(),
            "a failed sweep must fail create_from so the swap stays in its non-terminal refund state"
        );
        assert_eq!(
            client
                .lock()
                .await
                .sweep_all_invocations
                .load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            client
                .lock()
                .await
                .open_wallet_invocations
                .load(Ordering::SeqCst),
            1,
            "the main wallet must be re-opened even after a failed sweep"
        );
    }

    #[tokio::test]
    async fn refund_sweep_refresh_failure_propagates() {
        let client = Mutex::new(DummyClient::for_sweep(vec![Err(())], vec![], vec![]));

        let (spend_key, view_key, restore_height) = sweep_test_inputs();
        let result = create_from_and_sweep(
            &client,
            Network::Stagenet,
            "asb-wallet",
            "52UNVoFakeMainAddress",
            "2a779092".to_owned(),
            spend_key,
            view_key,
            restore_height,
        )
        .await;

        assert!(
            result.is_err(),
            "a failed refresh must fail create_from so the swap stays in its non-terminal refund state"
        );
        assert_eq!(
            client
                .lock()
                .await
                .sweep_all_invocations
                .load(Ordering::SeqCst),
            0
        );
        assert_eq!(
            client
                .lock()
                .await
                .open_wallet_invocations
                .load(Ordering::SeqCst),
            1,
            "the main wallet must be re-opened even after a failed refresh"
        );
    }

    #[tokio::test]
    async fn refund_sweep_is_skipped_when_wallet_is_already_empty() {
        let client = Mutex::new(DummyClient::for_sweep(
            vec![Ok(wallet::Refreshed {
                blocks_fetched: 0,
                received_money: false,
            })],
            vec![Ok(wallet::GetBalance {
                balance: 0,
                unlocked_balance: 0,
                multisig_import_needed: false,
                blocks_to_unlock: 0,
                time_to_unlock: 0,
            })],
            vec![],
        ));

        let (spend_key, view_key, restore_height) = sweep_test_inputs();
        let result = create_from_and_sweep(
            &client,
            Network::Stagenet,
            "asb-wallet",
            "52UNVoFakeMainAddress",
            "2a779092".to_owned(),
            spend_key,
            view_key,
            restore_height,
        )
        .await;

        assert!(
            result.is_ok(),
            "an already-swept (empty) wallet is the idempotent success case"
        );
        assert_eq!(
            client
                .lock()
                .await
                .sweep_all_invocations
                .load(Ordering::SeqCst),
            0,
            "no second spend may be issued for an already-swept wallet"
        );
    }

    #[tokio::test]
    async fn refund_sweep_success_sweeps_funded_wallet() {
        let client = Mutex::new(DummyClient::for_sweep(
            vec![Ok(wallet::Refreshed {
                blocks_fetched: 10,
                received_money: true,
            })],
            vec![Ok(wallet::GetBalance {
                balance: 222_051_868_248,
                unlocked_balance: 222_051_868_248,
                multisig_import_needed: false,
                blocks_to_unlock: 0,
                time_to_unlock: 0,
            })],
            vec![Ok(vec!["sweep-tx-hash".to_owned()])],
        ));

        let (spend_key, view_key, restore_height) = sweep_test_inputs();
        let result = create_from_and_sweep(
            &client,
            Network::Stagenet,
            "asb-wallet",
            "52UNVoFakeMainAddress",
            "2a779092".to_owned(),
            spend_key,
            view_key,
            restore_height,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(
            client
                .lock()
                .await
                .sweep_all_invocations
                .load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            client
                .lock()
                .await
                .open_wallet_invocations
                .load(Ordering::SeqCst),
            1,
            "the main wallet must be re-opened after a successful sweep"
        );
    }

    /// Reproduces the stranded-swap scenario: the lock transfer broadcast but
    /// the process died (or the RPC errored after submit) before the proof
    /// was persisted. Reconciliation must find the exact transfer (amount +
    /// destination) in the wallet's outgoing history.
    #[test]
    fn reconciliation_finds_the_broadcast_lock_transfer() {
        let request = test_transfer_request(10_000);
        let destination = destination_address(Network::Stagenet, &request);

        let mut transfers =
            test_transfers(vec![("reconciled-tx", 10_000, &destination.to_string())]);
        transfers.out.get_mut("reconciled-tx").unwrap().txid = "reconciled-tx".to_owned();

        let txid = match_outgoing_transfer(&transfers, &request, &destination)
            .expect("the broadcast lock must be found");

        assert_eq!(
            txid.0, "reconciled-tx",
            "the matched transfer carries the broadcast tx's hash"
        );
    }

    #[test]
    fn reconciliation_falls_back_to_the_map_key_for_the_txid() {
        let request = test_transfer_request(10_000);
        let destination = destination_address(Network::Stagenet, &request);

        // daemons populate the entry's own txid field; others may not -
        // the map key is the authoritative txid
        let mut transfers =
            test_transfers(vec![("reconciled-tx", 10_000, &destination.to_string())]);
        transfers.out.get_mut("reconciled-tx").unwrap().txid = String::new();

        let txid = match_outgoing_transfer(&transfers, &request, &destination).unwrap();
        assert_eq!(txid.0, "reconciled-tx");
    }

    #[tokio::test]
    async fn persisted_sweep_txid_wait_retries_after_wallet_rpc_restart() {
        let mut client = DummyClient::with_transfer_responses(vec![], vec![Err(-32000), Ok(false)]);
        client.get_transfer_by_txid_confirmations = vec![0, 1];
        let client = Mutex::new(client);
        let txid = TxHash("sweep-after-restart".to_owned());

        wait_for_outgoing_sweep_txid_with(
            &client,
            txid,
            Duration::from_millis(1),
            Duration::from_secs(5),
        )
        .await
        .unwrap();

        assert_eq!(
            client
                .lock()
                .await
                .get_transfer_by_txid_invocations
                .load(Ordering::SeqCst),
            2,
            "the wait must retry after the wallet-RPC lookup failure"
        );
    }

    #[test]
    fn outgoing_transfer_matches_recipient_in_destinations() {
        let target = "52UNVoFakeMainAddress";
        let transfer = monero_rpc::wallet::TransferInfo {
            double_spend_seen: false,
            txid: "sweep".to_owned(),
            amount: 10_000,
            address: "55hM7mWLcMjNpTSZLmJ3LriDzLm8ZbxAaPHJzSeAskpyTXC6LiXyxsvJC94MnuiZ9P5LPq3D6ofThTdzciwSRRPK38Tqz3P".to_owned(),
            destinations: vec![monero_rpc::wallet::Destination {
                address: target.to_owned(),
                amount: 10_000,
            }],
            confirmations: 1,
            tx_key: None,
        };

        assert!(transfer_sends_to(&transfer, target));
        assert!(!transfer_sends_to(&transfer, "52UNVoWrongAddress"));
    }

    #[test]
    fn reconciliation_matches_pending_transfers_too() {
        let request = test_transfer_request(10_000);
        let destination = destination_address(Network::Stagenet, &request);

        let transfers = GetTransfers {
            out: Default::default(),
            pending: test_transfers(vec![("pending-tx", 10_000, &destination.to_string())]).out,
        };

        assert!(
            match_outgoing_transfer(&transfers, &request, &destination).is_some(),
            "a submitted-but-unconfirmed lock is a valid reconciliation target"
        );
    }

    #[test]
    fn reconciliation_ignores_transfers_to_other_destinations_or_amounts() {
        let request = test_transfer_request(10_000);
        let destination = destination_address(Network::Stagenet, &request);

        let wrong_amount = test_transfers(vec![("tx1", 10_001, &destination.to_string())]);
        let wrong_address = test_transfers(vec![("tx2", 10_000, "52UNVoFakeMainAddress")]);

        assert!(match_outgoing_transfer(&wrong_amount, &request, &destination).is_none());
        assert!(match_outgoing_transfer(&wrong_address, &request, &destination).is_none());
    }

    const TEST_KEY: &str = "0100000000000000000000000000000000000000000000000000000000000000";

    fn test_transfers(entries: Vec<(&str, u64, &str)>) -> GetTransfers {
        GetTransfers {
            out: entries
                .into_iter()
                .map(|(txid, amount, address)| {
                    (
                        txid.to_owned(),
                        monero_rpc::wallet::TransferInfo {
                            double_spend_seen: false,
                            txid: txid.to_owned(),
                            amount,
                            address: "55hM7mWLcMjNpTSZLmJ3LriDzLm8ZbxAaPHJzSeAskpyTXC6LiXyxsvJC94MnuiZ9P5LPq3D6ofThTdzciwSRRPK38Tqz3P".to_owned(),
                            destinations: vec![monero_rpc::wallet::Destination {
                                address: address.to_owned(),
                                amount,
                            }],
                            confirmations: 0,
                            tx_key: Some(
                                ::monero::PrivateKey::from_str(TEST_KEY)
                                    .expect("the test key must be a valid canonical hex scalar"),
                            ),
                        },
                    )
                })
                .collect(),
            pending: Default::default(),
        }
    }

    fn test_transfer_request(amount_piconero: u64) -> TransferRequest {
        let scalar = crate::monero::Scalar::random(&mut rand::thread_rng());
        let private_spend = PrivateKey::from_scalar(scalar);
        let private_view = crate::monero::PrivateViewKey::new_random(&mut rand::thread_rng());

        TransferRequest {
            public_spend_key: PublicKey::from_private_key(&private_spend),
            public_view_key: private_view.public(),
            amount: Amount::from_piconero(amount_piconero),
        }
    }

    #[test]
    fn constructed_tx_requires_metadata() {
        let tx = wallet::Transfer {
            amount: 1,
            fee: 1,
            multisig_txset: String::new(),
            tx_blob: String::new(),
            tx_hash: "abc".to_owned(),
            tx_key: None,
            tx_metadata: String::new(),
            unsigned_txset: String::new(),
        };

        assert!(ConstructedTx::try_from(tx).is_err());
    }

    #[test]
    fn constructed_tx_exposes_transfer_proof() {
        let tx = wallet::Transfer {
            amount: 1,
            fee: 1,
            multisig_txset: String::new(),
            tx_blob: "00".to_owned(),
            tx_hash: "abc".to_owned(),
            tx_key: Some(PrivateKey {
                scalar: crate::monero::Scalar::random(&mut rand::thread_rng()),
            }),
            tx_metadata: "metadata".to_owned(),
            unsigned_txset: String::new(),
        };

        let constructed = ConstructedTx::try_from(tx).unwrap();
        assert_eq!(
            constructed.transfer_proof().unwrap().tx_hash(),
            TxHash("abc".to_owned())
        );
    }
}

const MONITORING_WALLET_CONNECT_ATTEMPTS: u32 = 15;
const MONITORING_WALLET_CONNECT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether an error message (full chain) means the wallet RPC could not be
/// reached at all, as opposed to the RPC answering with an error.
fn is_wallet_rpc_connect_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("connection refused")
        || message.contains("tcp connect error")
        || message.contains("error sending request")
}

#[cfg(test)]
mod wallet_rpc_connect_retry_tests {
    use super::is_wallet_rpc_connect_error;

    #[test]
    fn a_refused_connection_is_retried() {
        assert!(is_wallet_rpc_connect_error(
            "Unable to create height-bounded Monero monitoring wallet: error sending request for url (http://127.0.0.1:27786/json_rpc): client error (Connect): tcp connect error: Connection refused (os error 61)"
        ));
    }

    #[test]
    fn an_rpc_level_error_is_not_retried() {
        assert!(!is_wallet_rpc_connect_error(
            "JSON-RPC request failed with code -1: Failed to open wallet : file not found"
        ));
        assert!(!is_wallet_rpc_connect_error("Invalid placeholder Monero spend key"));
    }
}
