use crate::{DojoFeeEstimator, DojoTxView};
use futures::future::BoxFuture;
use anyhow::{Context, Result};
use bitcoin::{FeeRate, ScriptBuf, Transaction, Txid};
use std::sync::Arc;
use std::time::Duration;

const DOJO_CONFIRMATION_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// An object that can estimate fee rates and minimum relay fees.
pub trait EstimateFeeRate {
    /// Estimate the fee rate for a given target block.
    fn estimate_feerate(
        &self,
        target_block: u32,
    ) -> impl std::future::Future<Output = Result<FeeRate>> + Send;
    /// Get the minimum relay fee.
    fn min_relay_fee(&self) -> impl std::future::Future<Output = Result<FeeRate>> + Send;
}

/// A subscription to the status of a given transaction
/// that can be used to wait for the transaction to be confirmed.
#[derive(Debug, Clone)]
pub struct Subscription {
    /// A receiver used to await updates to the status of the transaction.
    pub receiver: tokio::sync::watch::Receiver<ScriptStatus>,
    /// The number of confirmations we require for a transaction to be considered final.
    pub finality_confirmations: u32,
    /// The transaction ID we are subscribing to.
    pub txid: Txid,
    /// The paired Dojo's independent chain view, if configured.
    pub dojo: Option<Arc<DojoFeeEstimator>>,
    /// Checks a claimed inclusion with a merkle proof. The Dojo's word alone is
    /// never accepted: without a checker its view cannot end a wait (audit H-4).
    pub inclusion: Option<InclusionChecker>,
}

/// Proves (`Ok(true)`) or fails to prove (`Ok(false)`) that a transaction is in
/// the block at a given height; an error means the check could not be made.
#[derive(Clone)]
pub struct InclusionChecker(
    pub Arc<dyn Fn(Txid, u32) -> BoxFuture<'static, anyhow::Result<bool>> + Send + Sync>,
);

impl std::fmt::Debug for InclusionChecker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InclusionChecker")
    }
}

impl InclusionChecker {
    pub async fn check(&self, txid: Txid, height: u32) -> anyhow::Result<bool> {
        (self.0)(txid, height).await
    }
}

/// Whether the Dojo's report about `txid` may end a wait for `target`
/// confirmations: it must reach the target, name the block, and that block
/// must be proven to hold the transaction (audit H-4).
pub async fn dojo_claim_is_proven(
    txid: Txid,
    view: DojoTxView,
    target: u32,
    inclusion: Option<&InclusionChecker>,
) -> bool {
    if view.confirmations < target {
        tracing::debug!(%txid, confirmations = view.confirmations, target, "Dojo confirms fewer than the required confirmations; still waiting");
        return false;
    }
    let Some(height) = view.block_height else {
        tracing::warn!(%txid, "Dojo reports confirmations without a block height; cannot prove it, still waiting");
        return false;
    };
    let Some(inclusion) = inclusion else {
        tracing::warn!(%txid, "No merkle-proof check is available for the Dojo's report; still waiting");
        return false;
    };
    match inclusion.check(txid, height).await {
        Ok(true) => true,
        Ok(false) => {
            tracing::warn!(%txid, height, "The Dojo reports this transaction confirmed but no Electrum server proved it; still waiting");
            false
        }
        Err(error) => {
            tracing::debug!(%txid, height, "Could not check the Dojo's report with a merkle proof: {:#}", error);
            false
        }
    }
}

impl Subscription {
    pub fn new(
        receiver: tokio::sync::watch::Receiver<ScriptStatus>,
        finality_confirmations: u32,
        txid: Txid,
    ) -> Self {
        Self {
            receiver,
            finality_confirmations,
            txid,
            dojo: None,
            inclusion: None,
        }
    }

    pub fn with_inclusion_checker(mut self, inclusion: Option<InclusionChecker>) -> Self {
        self.inclusion = inclusion;
        self
    }

    pub fn with_dojo(mut self, dojo: Option<Arc<DojoFeeEstimator>>) -> Self {
        self.dojo = dojo;
        self
    }

    pub async fn wait_until_final(&self) -> anyhow::Result<()> {
        let conf_target = self.finality_confirmations;
        let txid = self.txid;

        tracing::info!(%txid, required_confirmation=%conf_target, "Waiting for Bitcoin transaction finality");

        let mut seen_confirmations = 0;

        self.wait_until_with_dojo(
            |status| match status {
                ScriptStatus::Confirmed(inner) => {
                    let confirmations = inner.confirmations();

                    if confirmations > seen_confirmations {
                        tracing::info!(%txid,
                        seen_confirmations = %confirmations,
                        needed_confirmations = %conf_target,
                        "Waiting for Bitcoin transaction finality");
                        seen_confirmations = confirmations;
                    }

                    inner.meets_target(conf_target)
                }
                _ => false,
            },
            conf_target,
        )
        .await
    }

    pub async fn wait_until_seen(&self) -> anyhow::Result<()> {
        self.wait_until_with_dojo(ScriptStatus::has_been_seen, 1)
            .await
    }

    pub async fn wait_until_confirmed_with<T>(&self, target: T) -> anyhow::Result<()>
    where
        T: Into<u32>,
        T: Copy,
    {
        let target = target.into();
        self.wait_until_with_dojo(|status| status.is_confirmed_with(target), target)
            .await
    }

    pub async fn wait_until(
        &self,
        mut predicate: impl FnMut(&ScriptStatus) -> bool,
    ) -> anyhow::Result<()> {
        let mut receiver = self.receiver.clone();

        while !predicate(&receiver.borrow()) {
            receiver
                .changed()
                .await
                .map_err(|_| anyhow::anyhow!("Failed while waiting for next status update"))?;
        }

        Ok(())
    }

    /// Wait on Electrum, but also accept the paired Dojo's independent chain
    /// view once it reaches `independent_target` confirmations.
    async fn wait_until_with_dojo(
        &self,
        mut predicate: impl FnMut(&ScriptStatus) -> bool,
        independent_target: u32,
    ) -> anyhow::Result<()> {
        let mut receiver = self.receiver.clone();
        let electrum = async {
            while !predicate(&receiver.borrow()) {
                receiver
                    .changed()
                    .await
                    .context("Failed while waiting for next status update")?;
            }
            Ok(())
        };
        tokio::pin!(electrum);

        let Some(dojo) = self.dojo.clone() else {
            return electrum.await;
        };
        let mut interval = tokio::time::interval(DOJO_CONFIRMATION_POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                result = &mut electrum => return result,
                _ = interval.tick() => {
                    match dojo.tx_chain_view(&self.txid.to_string()).await {
                        Ok(view) => {
                            if dojo_claim_is_proven(self.txid, view, independent_target, self.inclusion.as_ref()).await {
                                tracing::warn!(
                                    %self.txid,
                                    confirmations = view.confirmations,
                                    target = independent_target,
                                    "Electrum confirmation feed is lagging; accepted the paired Dojo's confirmation after a merkle proof"
                                );
                                return Ok(());
                            }
                        }
                        Err(error) => {
                            tracing::debug!(
                                %self.txid,
                                "Independent Dojo confirmation check failed: {:#}",
                                error
                            );
                        }
                    }
                }
            }
        }
    }
}

/// The possible statuses of a script.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ScriptStatus {
    Unseen,
    InMempool,
    Confirmed(Confirmed),
    Retrying,
}

/// The status of a confirmed transaction.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Confirmed {
    /// The depth of this transaction within the blockchain.
    ///
    /// Zero if the transaction is included in the latest block.
    pub depth: u32,
}

impl Confirmed {
    pub fn new(depth: u32) -> Self {
        Self { depth }
    }

    /// Compute the depth of a transaction based on its inclusion height and the
    /// latest known block.
    ///
    /// Our information about the latest block might be outdated. To avoid an
    /// overflow, we make sure the depth is 0 in case the inclusion height
    /// exceeds our latest known block,
    pub fn from_inclusion_and_latest_block(inclusion_height: u32, latest_block: u32) -> Self {
        let depth = latest_block.saturating_sub(inclusion_height);

        Self { depth }
    }

    pub fn confirmations(&self) -> u32 {
        self.depth + 1
    }

    pub fn meets_target<T>(&self, target: T) -> bool
    where
        T: Into<u32>,
    {
        self.confirmations() >= target.into()
    }

    pub fn blocks_left_until<T>(&self, target: T) -> u32
    where
        T: Into<u32> + Copy,
    {
        if self.meets_target(target) {
            0
        } else {
            target.into() - self.confirmations()
        }
    }
}

impl std::fmt::Display for ScriptStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScriptStatus::Unseen => write!(f, "unseen"),
            ScriptStatus::InMempool => write!(f, "in mempool"),
            ScriptStatus::Retrying => write!(f, "retrying"),
            ScriptStatus::Confirmed(inner) => {
                write!(f, "confirmed with {} blocks", inner.confirmations())
            }
        }
    }
}

/// Defines a watchable transaction.
///
/// For a transaction to be watchable, we need to know two things: Its
/// transaction ID and the specific output script that is going to change.
/// A transaction can obviously have multiple outputs but our protocol purposes,
/// we are usually interested in a specific one.
pub trait Watchable: Send + Sync {
    /// The transaction ID.
    fn id(&self) -> Txid;
    /// The script of the output we are interested in.
    fn script(&self) -> ScriptBuf;
    /// Convenience method to get both the script and the txid.
    fn script_and_txid(&self) -> (ScriptBuf, Txid) {
        (self.script(), self.id())
    }
}

impl Watchable for (Txid, ScriptBuf) {
    fn id(&self) -> Txid {
        self.0
    }

    fn script(&self) -> ScriptBuf {
        self.1.clone()
    }
}

// Watching a transaction by watching it's first output
// (because Electrum expects a script hash). This works
// because all outputs of a transaction have the same status.
impl Watchable for Transaction {
    fn id(&self) -> Txid {
        self.compute_txid()
    }

    fn script(&self) -> ScriptBuf {
        debug_assert!(!self.output.is_empty(), "Transaction has no outputs");
        self.output[0].script_pubkey.clone()
    }
}

impl Watchable for &dyn Watchable {
    fn id(&self) -> Txid {
        (*self).id()
    }

    fn script(&self) -> ScriptBuf {
        (*self).script()
    }
}

impl Watchable for Box<dyn Watchable> {
    fn id(&self) -> Txid {
        (**self).id()
    }

    fn script(&self) -> ScriptBuf {
        (**self).script()
    }
}

impl ScriptStatus {
    pub fn from_confirmations(confirmations: u32) -> Self {
        match confirmations {
            0 => Self::InMempool,
            confirmations => Self::Confirmed(Confirmed::new(confirmations - 1)),
        }
    }
}

impl ScriptStatus {
    /// Check if the script has any confirmations.
    pub fn is_confirmed(&self) -> bool {
        matches!(self, ScriptStatus::Confirmed(_))
    }

    /// Check if the script has met the given confirmation target.
    pub fn is_confirmed_with<T>(&self, target: T) -> bool
    where
        T: Into<u32>,
    {
        match self {
            ScriptStatus::Confirmed(inner) => inner.meets_target(target),
            _ => false,
        }
    }

    // Calculate the number of blocks left until the target is met.
    pub fn blocks_left_until<T>(&self, target: T) -> u32
    where
        T: Into<u32> + Copy,
    {
        match self {
            ScriptStatus::Confirmed(inner) => inner.blocks_left_until(target),
            _ => target.into(),
        }
    }

    pub fn has_been_seen(&self) -> bool {
        matches!(self, ScriptStatus::InMempool | ScriptStatus::Confirmed(_))
    }

    pub fn confirmations(&self) -> u32 {
        match self {
            ScriptStatus::Unseen => 0,
            ScriptStatus::InMempool => 0,
            ScriptStatus::Retrying => 0,
            ScriptStatus::Confirmed(confirmed) => confirmed.confirmations(),
        }
    }
}
