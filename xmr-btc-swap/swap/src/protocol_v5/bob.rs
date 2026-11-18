use crate::protocol_v5::core::monero::{BlockHeight, TransferProof};
use crate::protocol_v5::p2p::protocols::encrypted_signature;
use crate::protocol_v5::store::ProtocolV5Store;
use crate::protocol_v5::{db, event_loop::BobEventLoopHandle, machine};
use anyhow::{bail, Context, Result};
use bitcoin::Transaction;
use futures::FutureExt;
use uuid::Uuid;

/// Build and persist Bob's signed Bitcoin lock before broadcasting it.
///
/// Recovery resumes from the stored `BtcLockReadyToPublish` state.
pub async fn prepare_lock(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state2: machine::bob::State2,
    monero_wallet_restore_blockheight: BlockHeight,
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
) -> Result<(machine::bob::State3, Transaction)> {
    let (state3, tx_lock) = state2.lock_btc().await?;
    let btc_lock_tx_signed = bitcoin_wallet
        .sign_and_finalize(tx_lock.clone().into())
        .await
        .context("Failed to sign the Bitcoin lock transaction")?;

    store
        .store_bob(
            swap_id,
            &db::bob::Bob::BtcLockReadyToPublish {
                btc_lock_tx_signed: btc_lock_tx_signed.clone(),
                state3: state3.clone(),
                monero_wallet_restore_blockheight,
                publish_deadline: None,
                broadcast_attempted: false,
            },
        )
        .await?;

    Ok((state3, btc_lock_tx_signed))
}

/// Broadcast the persisted Bitcoin lock and record that it is live.
pub async fn publish_lock(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    wallet: &crate::bitcoin::Wallet,
) -> Result<bitcoin::Txid> {
    let Some(db::bob::Bob::BtcLockReadyToPublish {
        btc_lock_tx_signed,
        state3,
        monero_wallet_restore_blockheight,
        ..
    }) = store.load_bob(swap_id).await?
    else {
        bail!("no signed protocol-5 Bitcoin lock is waiting to be published");
    };

    let lock_psbt: crate::protocol_v5::core::bitcoin::PartiallySignedTransaction =
        state3.tx_lock.clone().into();
    let lock_fee = lock_psbt.fee().ok();
    let lock_weight = btc_lock_tx_signed.weight();
    let lock_vsize = lock_weight.to_vbytes_ceil();
    let lock_fee_sats = lock_fee.map(|fee| fee.to_sat());
    let effective_sat_per_vb = lock_fee
        .map(|fee| fee.to_sat() as f64 / lock_vsize as f64);

    let (txid, _subscription) =
        bitcoin_wallet::BitcoinWallet::ensure_broadcasted(&**wallet, btc_lock_tx_signed, "lock")
            .await?;

    tracing::info!(
        %txid,
        ?lock_fee_sats,
        ?lock_weight,
        vsize = lock_vsize,
        ?effective_sat_per_vb,
        "V5_BOB_LOCK_PUBLISHED"
    );

    store
        .store_bob(
            swap_id,
            &db::bob::Bob::BtcLocked {
                state3,
                monero_wallet_restore_blockheight,
            },
        )
        .await?;

    Ok(txid)
}

/// Verify the Monero lock proof against the chain and advance to `State4`.
///
/// Returns `Ok(None)` when the transfer is not visible/valid yet; the caller
/// can retry. This does not send anything to Alice.
pub async fn verify_and_accept_xmr_lock(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state3: machine::bob::State3,
    lock_transfer_proof: TransferProof,
    monero_wallet_restore_blockheight: BlockHeight,
    hermes_amount: Option<crate::protocol_v5::core::monero::Amount>,
    monero_daemon: &crate::monero::ng::Daemon,
) -> Result<Option<machine::bob::State4>> {
    let (public_spend_key, private_view_key) = state3.xmr_view_keys();
    let verified = monero_daemon
        .verify_protocol_transfer(
            &lock_transfer_proof.tx_hash.0,
            &public_spend_key.as_bytes(),
            &private_view_key.0.as_bytes(),
            state3.xmr_amount().as_pico(),
        )
        .await?;

    if !verified {
        return Ok(None);
    }

    if !witnesses_agree(
        monero_daemon.witnesses().as_ref(),
        monero_daemon.witnesses_required(),
        &lock_transfer_proof.tx_hash.0,
        &public_spend_key.as_bytes(),
        &private_view_key.0.as_bytes(),
        state3.xmr_amount().as_pico(),
    )
    .await?
    {
        return Ok(None);
    }

    accept_xmr_lock(
        store,
        swap_id,
        state3,
        lock_transfer_proof,
        monero_wallet_restore_blockheight,
        hermes_amount,
    )
    .await
    .map(Some)
}

/// Verify the lock proof through wallet-RPC and advance to `State4`.
///
/// This avoids a separate Monero daemon dependency for the JNI path.
pub async fn verify_and_accept_xmr_lock_with_wallet(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state3: machine::bob::State3,
    lock_transfer_proof: TransferProof,
    monero_wallet_restore_blockheight: BlockHeight,
    hermes_amount: Option<crate::protocol_v5::core::monero::Amount>,
    monero_wallet: &crate::monero::Wallet,
    monero_daemon: Option<&crate::monero::ng::Daemon>,
    network: ::monero::Network,
) -> Result<Option<machine::bob::State4>> {
    let (public_spend_key, private_view_key) = state3.xmr_view_keys();
    let public_spend_key = ::monero::PublicKey::from_slice(&public_spend_key.as_bytes())
        .map_err(|error| anyhow::anyhow!("invalid protocol public spend key: {error}"))?;
    let private_view_key = crate::monero::PrivateViewKey::from_private_key(
        ::monero::PrivateKey::from_slice(&private_view_key.0.as_bytes())
            .map_err(|error| anyhow::anyhow!("invalid protocol private view key: {error}"))?,
    );
    let address =
        ::monero::Address::standard(network, public_spend_key, private_view_key.public().into());
    let proof = crate::monero::TransferProof::new(
        crate::monero::TxHash(lock_transfer_proof.tx_hash.0.clone()),
        ::monero::PrivateKey::from_slice(&lock_transfer_proof.tx_key.as_bytes())
            .map_err(|error| anyhow::anyhow!("invalid transfer proof key: {error}"))?,
    );

    let verified = monero_wallet
        .verify_transfer_to(
            &proof,
            &address,
            crate::monero::Amount::from_piconero(state3.xmr_amount().as_pico()),
        )
        .await?;

    if !verified {
        return Ok(None);
    }

    // `check_tx_key` sums every output to the address and ignores the
    // transaction's unlock time. A lock that pays the right amount but cannot be
    // spent (far-future unlock time) or that is split into several small
    // outputs would pass it, and Bob would reveal his redeem signature for XMR
    // he cannot sweep. The sweep spends the single largest unlocked output, so
    // require exactly that through the daemon. Fail closed: without a daemon the
    // lock cannot be proven spendable, and Bob must not send the signature.
    let monero_daemon = monero_daemon.ok_or_else(|| {
        anyhow::anyhow!(
            "A Monero daemon is required to verify that Alice's lock is spendable; not sending the signature"
        )
    })?;
    let (spend_key_bytes, view_key_bytes) = state3.xmr_view_keys();
    let spendable = monero_daemon
        .has_spendable_lock_output(
            &lock_transfer_proof.tx_hash.0,
            &spend_key_bytes.as_bytes(),
            &view_key_bytes.0.as_bytes(),
            state3.xmr_amount().as_pico(),
        )
        .await?;
    anyhow::ensure!(
        spendable,
        "Alice's Monero lock is not spendable: it has no unlocked output of the agreed amount (timelocked, split or too small); not sending the signature"
    );

    // The main node's word is not enough: every witness node must agree too.
    if !witnesses_agree(
        monero_daemon.witnesses().as_ref(),
        monero_daemon.witnesses_required(),
        &lock_transfer_proof.tx_hash.0,
        &spend_key_bytes.as_bytes(),
        &view_key_bytes.0.as_bytes(),
        state3.xmr_amount().as_pico(),
    )
    .await?
    {
        return Ok(None);
    }

    accept_xmr_lock(
        store,
        swap_id,
        state3,
        lock_transfer_proof,
        monero_wallet_restore_blockheight,
        hermes_amount,
    )
    .await
    .map(Some)
}

/// Whether every witness node agrees with the main node that Alice's lock is valid. `None` or an
/// empty set means no witnesses are configured. A disagreement is `false`; a witness that cannot be
/// reached is an error, never a pass: the caller retries, and the cancel timelock bounds the wait.
pub(crate) async fn witnesses_agree(
    witnesses: Option<&crate::monero::witness::Witnesses>,
    required: bool,
    tx_hash: &str,
    public_spend_key: &[u8; 32],
    private_view_key: &[u8],
    amount_pico: u64,
) -> Result<bool> {
    use crate::monero::witness::WitnessVerdict;
    let Some(witnesses) = witnesses.filter(|witnesses| !witnesses.is_empty()) else {
        // Required but none configured (too few nodes were available): fail closed.
        anyhow::ensure!(
            !required,
            "Monero witness nodes are required for this swap but none are configured; not sending the signature"
        );
        return Ok(true);
    };
    match witnesses
        .verify_lock(tx_hash, public_spend_key, private_view_key, amount_pico)
        .await
    {
        WitnessVerdict::Agree => {
            tracing::info!(witnesses = ?witnesses.names(), "V5_BOB_WITNESSES_AGREE");
            Ok(true)
        }
        WitnessVerdict::Disagree { witness } => {
            tracing::warn!(%witness, tx_hash, "V5_BOB_WITNESS_DISAGREES: a witness node does not see Alice's Monero lock paying the agreed amount");
            Ok(false)
        }
        WitnessVerdict::Unreachable { witness, reason } => {
            bail!("Witness Monero node {witness} could not confirm Alice's lock ({reason}); not sending the signature")
        }
    }
}

/// Advance to `State4` after the caller has verified the Monero lock, and
/// persist it before any signature is sent to Alice.
pub async fn accept_xmr_lock(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state3: machine::bob::State3,
    lock_transfer_proof: TransferProof,
    monero_wallet_restore_blockheight: BlockHeight,
    hermes_amount: Option<crate::protocol_v5::core::monero::Amount>,
) -> Result<machine::bob::State4> {
    let state4 = state3.xmr_locked(
        monero_wallet_restore_blockheight,
        lock_transfer_proof.into(),
        hermes_amount,
    );

    store
        .store_bob(
            swap_id,
            &db::bob::Bob::XmrLocked {
                state4: state4.clone(),
            },
        )
        .await?;

    Ok(state4)
}

/// Send Bob's encrypted redeem signature and persist the sent state.
pub async fn send_redeem_encrypted_signature(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state4: machine::bob::State4,
    handle: &BobEventLoopHandle,
) -> Result<()> {
    handle
        .send_encrypted_signature(encrypted_signature::Request {
            swap_id,
            tx_redeem_encsig: state4.tx_redeem_encsig(),
        })
        .await?;

    store
        .store_bob(
            swap_id,
            &db::bob::Bob::EncSigSent {
                state4,
                hermes_tx: None,
            },
        )
        .await?;

    Ok(())
}

/// Persist the cancel path after the lock is no longer redeemable.
pub async fn cancel_lock(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state3: machine::bob::State3,
    monero_wallet_restore_blockheight: BlockHeight,
) -> Result<machine::bob::State6> {
    let state6 = state3.cancel(monero_wallet_restore_blockheight);

    store
        .store_bob(swap_id, &db::bob::Bob::BtcCancelled(state6.clone()))
        .await?;

    Ok(state6)
}

/// Look for Alice's Bitcoin redeem and persist the recovered XMR keys.
pub async fn check_for_btc_redeem(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state4: &machine::bob::State4,
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
) -> Result<Option<machine::bob::State5>> {
    let Some(state5) = state4.check_for_tx_redeem(bitcoin_wallet).await? else {
        return Ok(None);
    };

    store
        .store_bob(swap_id, &db::bob::Bob::BtcRedeemed(state5.clone()))
        .await?;

    Ok(Some(state5))
}

/// Persist the cancel path after the XMR lock was accepted.
pub async fn cancel_after_lock(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state4: machine::bob::State4,
) -> Result<machine::bob::State6> {
    let state6 = state4.cancel();

    store
        .store_bob(swap_id, &db::bob::Bob::BtcCancelled(state6.clone()))
        .await?;

    Ok(state6)
}

pub enum BobOutcome {
    XmrRedeemed(machine::bob::State5),
    Cancelled(machine::bob::State6),
    Pending,
}

/// One observation pass over the Bitcoin chain after the XMR lock.
///
/// Returns `Pending` while the redeem is not visible and the cancel timelock
/// has not expired; the caller polls this and persists transitions internally.
pub async fn observe_btc_outcome(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state4: &machine::bob::State4,
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
) -> Result<BobOutcome> {
    if let Some(state5) = check_for_btc_redeem(store, swap_id, state4, bitcoin_wallet).await? {
        return Ok(BobOutcome::XmrRedeemed(state5));
    }

    if state4
        .expired_timelock(bitcoin_wallet)
        .await?
        .cancel_timelock_expired()
    {
        let state6 = cancel_after_lock(store, swap_id, state4.clone()).await?;
        return Ok(BobOutcome::Cancelled(state6));
    }

    Ok(BobOutcome::Pending)
}

/// Key the redeem wallet file by its restore height: `generate_from_keys`
/// refuses to overwrite an existing wallet, so a fixed name makes every retry
/// reopen the old wallet at the old (stale) height and rescan it again.
fn redeem_wallet_name(swap_id: Uuid, restore_height: u32) -> String {
    format!("{swap_id}-redeem-wallet-{restore_height}")
}

/// Redeem XMR from the recovered keys through wallet-RPC, then persist completion.
pub async fn redeem_xmr_with_wallet(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state5: &machine::bob::State5,
    monero_wallet: &crate::monero::Wallet,
    destination: ::monero::Address,
    restore_height: Option<u32>,
    on_sync: impl FnOnce(),
    on_sweep: impl FnOnce(),
) -> Result<Vec<crate::monero::TxHash>> {
    let (spend_key, view_key) = state5.xmr_keys();
    let spend_key = ::monero::PrivateKey::from_slice(&spend_key.as_bytes())
        .map_err(|error| anyhow::anyhow!("invalid recovered XMR spend key: {error}"))?;
    let view_key = crate::monero::PrivateViewKey::from_private_key(
        ::monero::PrivateKey::from_slice(&view_key.0.as_bytes())
            .map_err(|error| anyhow::anyhow!("invalid recovered XMR view key: {error}"))?,
    );

    // Prefer the XMR lock block when the caller could resolve it: a recorded
    // height can be thousands of blocks stale and turn the redeem into a full
    // rescan (WORKLOG §364).
    let restore_height = restore_height
        .unwrap_or(state5.monero_wallet_restore_blockheight.height as u32);
    // Key the wallet file by its restore height. generate_from_keys refuses to
    // overwrite an existing wallet, so a fixed name makes every retry reopen
    // the old wallet at the old (stale) height and rescan it again.
    let wallet_name = redeem_wallet_name(swap_id, restore_height);
    let txids = monero_wallet
        .redeem_from_keys(
            wallet_name,
            spend_key,
            view_key,
            monero_rpc::wallet::BlockHeight { height: restore_height },
            destination,
            on_sync,
            on_sweep,
        )
        .await?;

    store
        .store_bob(
            swap_id,
            &db::bob::Bob::Done(db::bob::BobEndState::XmrRedeemed {
                tx_lock_id: state5.tx_lock_id(),
            }),
        )
        .await?;

    Ok(txids)
}

/// Build the signed XMR sweep from the recovered wallet keys.
pub async fn prepare_xmr_redeem(
    monero_daemon: &crate::monero::ng::Daemon,
    state5: &machine::bob::State5,
    destination: &str,
    network: ::monero::Network,
) -> Result<crate::monero::ng::ConstructedSweep> {
    let (spend_key, view_key) = state5.xmr_keys();
    let spend_key = ::monero::PrivateKey::from_slice(&spend_key.as_bytes())
        .map_err(|error| anyhow::anyhow!("invalid recovered XMR spend key: {error}"))?;
    let view_key = crate::monero::PrivateViewKey::from_private_key(
        ::monero::PrivateKey::from_slice(&view_key.0.as_bytes())
            .map_err(|error| anyhow::anyhow!("invalid recovered XMR view key: {error}"))?,
    );
    let destination = destination
        .parse()
        .map_err(|error| anyhow::anyhow!("invalid XMR redeem address: {error}"))?;
    let txid = crate::monero::TxHash(state5.lock_transfer_proof.tx_hash().0);

    monero_daemon
        .construct_sweep_to_single(&spend_key, &view_key, &txid, &destination, network)
        .await
}

/// What the buyer gets from the XMR sweep, in piconero (matrix N-15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XmrReceived {
    pub received_piconero: u64,
    pub fee_piconero: u64,
}

/// The seller's lock is only accepted if it has an output of exactly the agreed amount
/// (`monero-wallet-ng::verify`), and the sweep spends that output to one destination, so the
/// buyer receives the agreed amount minus the sweep's public network fee.
pub fn xmr_received(
    state5: &machine::bob::State5,
    sweep: &crate::monero::ng::ConstructedSweep,
) -> Result<XmrReceived> {
    received_after_fee(state5.xmr_amount().as_pico(), sweep.fee_piconero()?)
}

/// A fee above the locked amount means the numbers are wrong: say nothing rather than guess.
pub fn received_after_fee(locked_piconero: u64, fee_piconero: u64) -> Result<XmrReceived> {
    let received_piconero = locked_piconero.checked_sub(fee_piconero).ok_or_else(|| {
        anyhow::anyhow!(
            "sweep fee {fee_piconero} exceeds the locked {locked_piconero} piconero"
        )
    })?;
    Ok(XmrReceived {
        received_piconero,
        fee_piconero,
    })
}

const SWEEP_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
/// About 30 minutes: far beyond normal Monero block times.
const SWEEP_MAX_POLLS: u32 = 180;

/// Whether the wait for the sweep's block logs a line on this poll: once a minute (the polls are
/// 10 s apart), never on the first poll.
fn sweep_wait_log_due(poll: u32) -> bool {
    poll > 0 && poll % 6 == 0
}

/// Poll until `poll` reports a block height. Query errors count as "not yet".
pub(crate) async fn wait_until_mined<F, Fut>(
    mut poll: F,
    interval: std::time::Duration,
    max_polls: u32,
) -> Result<u64>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<u64>>>,
{
    for attempt in 0..max_polls {
        if let Ok(Some(height)) = poll().await {
            return Ok(height);
        }
        if attempt + 1 < max_polls {
            tokio::time::sleep(interval).await;
        }
    }
    bail!("transaction was not mined after {max_polls} checks")
}

/// What to do about a signed Monero sweep that an earlier attempt may have left behind
/// (matrix S-03). Building a second sweep after a crash is never right if the first one was
/// accepted: both spend the same output, so the second is rejected while the swap shows an
/// error even though the Monero arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoredSweepPlan {
    /// No signed sweep was stored: build one.
    BuildNew,
    /// It is already in a block: only record the swap as done.
    Finish,
    /// The daemon has it and a block is pending: wait for it.
    Wait,
    /// The daemon does not know it (never sent, or dropped): send the same transaction again.
    Rebroadcast,
}

pub fn plan_stored_sweep(
    stored: bool,
    status: Option<crate::monero::ng::TxStatus>,
) -> StoredSweepPlan {
    use crate::monero::ng::TxStatus;
    if !stored {
        return StoredSweepPlan::BuildNew;
    }
    match status {
        Some(TxStatus::Mined(_)) => StoredSweepPlan::Finish,
        Some(TxStatus::InPool) => StoredSweepPlan::Wait,
        // a failed lookup is treated like "unknown": the resend then fails the same way
        // if the daemon is unreachable, and the caller builds a new sweep only after that
        Some(TxStatus::Unknown) | None => StoredSweepPlan::Rebroadcast,
    }
}

/// The signed sweep saved before publishing, if an earlier attempt got that far.
pub async fn stored_sweep(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
) -> Result<Option<crate::monero::ng::ConstructedSweep>> {
    Ok(match store.load_bob(swap_id).await? {
        Some(
            db::bob::Bob::XmrRedeemConstructed { xmr_redeem_tx, .. }
            | db::bob::Bob::XmrRedeemPublished { xmr_redeem_tx, .. },
        ) => Some(crate::monero::ng::ConstructedSweep::from_transaction(xmr_redeem_tx)),
        _ => None,
    })
}

/// Publish the prepared XMR sweep and record the completed swap.
///
/// The signed sweep is saved first: if the app stops after the daemon accepted it, the next
/// attempt sends that same transaction again instead of building a second one.
pub async fn publish_xmr_redeem(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state5: &machine::bob::State5,
    monero_daemon: &crate::monero::ng::Daemon,
    sweep: &crate::monero::ng::ConstructedSweep,
    on_published: impl FnOnce(&str),
) -> Result<crate::monero::TxHash> {
    store
        .store_bob(
            swap_id,
            &db::bob::Bob::XmrRedeemConstructed {
                state: state5.clone(),
                xmr_redeem_tx: sweep.transaction()?,
            },
        )
        .await?;

    monero_daemon
        .publish_raw_transaction(&sweep.tx_hex)
        .await?;

    finish_xmr_redeem(store, swap_id, state5, monero_daemon, sweep, on_published).await
}

/// Resume a redeem whose signed sweep an earlier attempt stored. `Ok(None)` means there is
/// nothing usable (none stored, or the daemon refused it) and the caller builds a new one.
pub async fn resend_stored_xmr_redeem(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state5: &machine::bob::State5,
    monero_daemon: &crate::monero::ng::Daemon,
    on_published: impl FnOnce(&str),
) -> Result<Option<crate::monero::TxHash>> {
    let Some(sweep) = stored_sweep(store, swap_id).await? else {
        return Ok(None);
    };
    let status = monero_daemon.tx_status(&sweep.tx_hash.0).await.ok();
    match plan_stored_sweep(true, status) {
        StoredSweepPlan::BuildNew => return Ok(None),
        StoredSweepPlan::Finish | StoredSweepPlan::Wait => {}
        StoredSweepPlan::Rebroadcast => {
            if let Err(error) = monero_daemon.publish_raw_transaction(&sweep.tx_hex).await {
                tracing::warn!(
                    %swap_id,
                    txid = %sweep.tx_hash.0,
                    "The stored Monero sweep was refused ({error:#}); building a new one"
                );
                return Ok(None);
            }
        }
    }
    tracing::info!(%swap_id, txid = %sweep.tx_hash.0, ?status, "V5_BOB_XMR_SWEEP_RESUMED");
    finish_xmr_redeem(store, swap_id, state5, monero_daemon, &sweep, on_published)
        .await
        .map(Some)
}

/// Wait for the published sweep to be mined, then record the swap as done.
async fn finish_xmr_redeem(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state5: &machine::bob::State5,
    monero_daemon: &crate::monero::ng::Daemon,
    sweep: &crate::monero::ng::ConstructedSweep,
    on_published: impl FnOnce(&str),
) -> Result<crate::monero::TxHash> {
    // The daemon has the sweep (or already had it): from here only a block is missing.
    tracing::info!(txid = %sweep.tx_hash.0, "V5_BOB_XMR_SWEEP_BROADCAST");
    on_published(&sweep.tx_hash.0);

    // A daemon can accept a transaction and later drop it from the pool. Only
    // record the swap as done once the sweep is in a block; until then the
    // swap stays resumable (audit first-review #2).
    let txid = sweep.tx_hash.0.clone();
    let polls = std::sync::atomic::AtomicU32::new(0);
    let started = std::time::Instant::now();
    let mined_at = wait_until_mined(
        || {
            let txid = txid.clone();
            let poll = polls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if sweep_wait_log_due(poll) {
                tracing::info!(%txid, waited_secs = started.elapsed().as_secs(), "V5_BOB_XMR_SWEEP_WAITING");
            }
            async move { monero_daemon.tx_block_height(&txid).await }
        },
        SWEEP_POLL_INTERVAL,
        SWEEP_MAX_POLLS,
    )
    .await
    .context("the Monero sweep was broadcast but has not been mined; the swap is not marked done, resume it to retry")?;
    tracing::info!(%txid, block = mined_at, "V5_BOB_XMR_SWEEP_MINED");

    store
        .store_bob(
            swap_id,
            &db::bob::Bob::Done(db::bob::BobEndState::XmrRedeemed {
                tx_lock_id: state5.tx_lock_id(),
            }),
        )
        .await?;

    Ok(sweep.tx_hash.clone())
}

/// Wait for Alice's transfer proof, verify it, acknowledge it, and send the
/// redeem signature. Returns the persisted `State4`.
pub async fn run_until_signature_sent(
    store: &dyn ProtocolV5Store,
    handle: &mut BobEventLoopHandle,
    swap_id: Uuid,
    state3: machine::bob::State3,
    on_xmr_lock_accepted: impl FnOnce(&machine::bob::State4),
    on_xmr_lock_confirmation: impl Fn(&str, u64),
    on_btc_lock_confirmed: impl FnOnce(),
    on_early_refund: impl FnOnce(),
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
    bitcoin_finality_confirmations: u32,
    monero_wallet_restore_blockheight: BlockHeight,
    monero_wallet: &crate::monero::Wallet,
    monero_daemon: Option<&crate::monero::ng::Daemon>,
    network: ::monero::Network,
    monero_finality_confirmations: u64,
) -> Result<Option<machine::bob::State4>> {
    // Wait for Alice's transfer proof, bounded by the chain: once the Bitcoin
    // lock reaches the cancel timelock, Bob can cancel/refund. Wall-clock
    // timeouts are wrong here — the lock confirmation itself can take longer
    // than any arbitrary timer (legacy used the same timelock bound).
    //
    // Report the lock confirmation to the UI while waiting; doing it after the
    // proof left the step stuck on "confirming" for the whole window.
    //
    // Watch for Alice's pre-signed early refund as well: v5 Alice may publish
    // it when the lock is not final in time, and Bob must not keep waiting for
    // a transfer proof that will never come.
    let early_refund = state3.construct_tx_early_refund();
    let early_refund_txid = early_refund.txid();
    let request = {
        let tx_lock_status = bitcoin_wallet
            .subscribe_to(Box::new(state3.tx_lock.clone()))
            .await;
        let early_refund_status = bitcoin_wallet
            .subscribe_to(Box::new(early_refund))
            .await;
        let mut proof = Box::pin(handle.recv_transfer_proof());
        let mut lock_confirms =
            Box::pin(tx_lock_status.wait_until_confirmed_with(bitcoin_finality_confirmations));
        let mut cancel_expires = Box::pin(wait_for_cancel_timelock(&state3, bitcoin_wallet));
        // Act on the early refund only once it is confirmed. "Seen" can be a lie from one server
        // or a transaction later evicted; acting on it stopped the transfer-proof wait and left
        // nothing but a wait with no deadline (audit 2026-11-10, H-3). Until it confirms, the
        // proof and the cancel timelock below stay in charge.
        let mut early_refund_confirmed =
            Box::pin(early_refund_status.wait_until_confirmed_with(1u32));
        let mut notified_lock_confirmed = false;
        let mut on_btc_lock_confirmed = Some(on_btc_lock_confirmed);
        let mut on_early_refund = Some(on_early_refund);

        loop {
            tokio::select! {
                request = &mut proof => {
                    // A ready proof can win the select even when confirmation is
                    // already observable. Poll once without delaying proof handling.
                    if !notified_lock_confirmed {
                        if let Some(Ok(())) = lock_confirms.as_mut().now_or_never() {
                            if let Some(callback) = on_btc_lock_confirmed.take() {
                                callback();
                            }
                        }
                    }
                    break request?;
                }
                expired = &mut cancel_expires => {
                    expired?;
                    bail!("Bitcoin cancel timelock expired before Alice's Monero transfer proof arrived");
                }
                confirmed = &mut lock_confirms, if !notified_lock_confirmed => {
                    confirmed?;
                    notified_lock_confirmed = true;
                    if let Some(callback) = on_btc_lock_confirmed.take() {
                        callback();
                    }
                }
                confirmed = &mut early_refund_confirmed => {
                    confirmed?;
                    tracing::info!(%early_refund_txid, "V5_BOB_EARLY_REFUND_CONFIRMED");
                    let state6 = state3.cancel(monero_wallet_restore_blockheight);
                    store
                        .store_bob(
                            swap_id,
                            &db::bob::Bob::BtcEarlyRefundPublished(state6),
                        )
                        .await?;
                    if let Some(callback) = on_early_refund.take() {
                        callback();
                    }
                    return Ok(None);
                }
            }
        }
    };

    anyhow::ensure!(
        request.swap_id == swap_id,
        "received a transfer proof for a different swap"
    );

    // Acknowledge receipt immediately so Alice's request-response future
    // completes. Verification below can wait up to 300s for the tx to become
    // visible, which exceeds Alice's request timeout and otherwise leaves her
    // retrying forever while Bob sits on an already-received proof.
    handle.ack_transfer_proof().await?;

    if let Some(daemon) = monero_daemon {
        // wallet-RPC's check_tx_key throws and stops its server when the
        // daemon has not seen the just-broadcast transaction yet. Poll the
        // daemon's REST lookup first so the wallet call runs against a
        // visible tx instead of failing the swap after ~4 seconds.
        daemon
            .wait_for_tx_visible(
                &request.tx_lock_proof.tx_hash.0,
                std::time::Duration::from_secs(300),
            )
            .await?;
    }

    let Some(state4) = verify_and_accept_xmr_lock_with_wallet(
        store,
        swap_id,
        state3,
        request.tx_lock_proof,
        monero_wallet_restore_blockheight,
        None,
        monero_wallet,
        monero_daemon,
        network,
    )
    .await?
    else {
        bail!("Alice's Monero lock could not be verified");
    };

    // Report the XMR lock as soon as the proof is accepted. Waiting until
    // after Monero finality left the UI on "BTC locked" for the whole
    // 10-confirmation window.
    handle.record_xmr_lock_accepted(state4.clone());
    on_xmr_lock_accepted(&state4);

    // Never reveal the redeem signature before the Monero lock is final:
    // Alice could otherwise redeem BTC on an XMR lock that reorgs away.
    wait_for_xmr_lock_finality(
        &state4,
        monero_wallet,
        monero_finality_confirmations,
        Some(on_xmr_lock_confirmation),
    )
    .await?;

    // ...and every witness node must also see the lock confirmed before the signature is revealed.
    witnesses_confirm_lock_for(monero_daemon, &state4, monero_finality_confirmations).await?;

    send_redeem_encrypted_signature(store, swap_id, state4.clone(), handle).await?;

    Ok(Some(state4))
}

/// Before the redeem signature is revealed, every witness node must also see Alice's Monero
/// lock reach `target` confirmations. Used by the main path and by every resume arm that sends
/// the signature (audit M-2: the resume arms skipped it). The Node Type setting decides the
/// witnesses: with only the user's own node answering there are none and none are required, so
/// there is nothing to check (`HelperMoneroWitnesses.select`).
pub async fn witnesses_confirm_lock_for(
    monero_daemon: Option<&crate::monero::ng::Daemon>,
    state4: &machine::bob::State4,
    target: u64,
) -> Result<()> {
    let Some(daemon) = monero_daemon else {
        return Ok(());
    };
    witnesses_confirm_lock(
        daemon.witnesses(),
        daemon.witnesses_required(),
        &state4.lock_transfer_proof().tx_hash().0,
        target,
        crate::monero::witness::WITNESS_POLL_INTERVAL,
        crate::monero::witness::WITNESS_CONFIRMATION_DEADLINE,
    )
    .await
}

/// [`witnesses_confirm_lock_for`] without the daemon, for tests.
pub(crate) async fn witnesses_confirm_lock(
    witnesses: Option<crate::monero::witness::Witnesses>,
    required: bool,
    tx_hash: &str,
    target: u64,
    poll: std::time::Duration,
    deadline: std::time::Duration,
) -> Result<()> {
    let witnesses = witnesses.filter(|witnesses| !witnesses.is_empty());
    anyhow::ensure!(
        witnesses.is_some() || !required,
        "Monero witness nodes are required for this swap but none are configured; not sending the signature"
    );
    if let Some(witnesses) = witnesses {
        witnesses.wait_confirmed(tx_hash, target, poll, deadline).await?;
    }
    Ok(())
}

/// Chain-based bound for the transfer-proof wait: returns once the Bitcoin
/// lock has reached the cancel timelock, at which point Bob can cancel/refund.
async fn wait_for_cancel_timelock(
    state3: &machine::bob::State3,
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
) -> Result<()> {
    loop {
        if !matches!(
            state3.expired_timelock(bitcoin_wallet).await?,
            crate::protocol_v5::core::bitcoin::ExpiredTimelocks::None { .. }
        ) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

/// Wait for the lock transaction to reach finality using `check_tx_key`.
///
/// The Bob monitoring wallet is a check-only wallet and may not own the shared
/// lock address, so `get_transfer_by_txid` can never see it. `check_tx_key`
/// takes the swap view key and address directly and therefore works without
/// ownership.
pub async fn wait_for_xmr_lock_finality<F>(
    state4: &machine::bob::State4,
    monero_wallet: &crate::monero::Wallet,
    monero_finality_confirmations: u64,
    on_confirmation: Option<F>,
) -> Result<()>
where
    F: Fn(&str, u64),
{
    let (public_spend, private_view) = state4
        .xmr_view_keys()
        .context("Alice's Monero public spend share is unavailable")?;
    let public_spend = ::monero::PublicKey::from_slice(&public_spend.as_bytes())
        .context("Invalid protocol Monero spend key")?;
    let private_view = crate::monero::PrivateViewKey::from_private_key(
        ::monero::PrivateKey::from_slice(&private_view.0.as_bytes())
            .context("Invalid protocol Monero view key")?,
    );
    let proof = state4.lock_transfer_proof();
    let tx_key = proof
        .tx_key
        .context("Monero lock proof has no transaction key")?;
    let tx_key = ::monero::PrivateKey::from_slice(&tx_key.as_bytes())
        .context("Invalid Monero lock transaction key")?;
    let transfer_proof =
        crate::monero::TransferProof::new(crate::monero::TxHash(proof.tx_hash.0), tx_key);
    monero_wallet
        .watch_for_transfer_with_progress(
            crate::monero::wallet::WatchRequest {
                conf_target: monero_finality_confirmations,
                public_view_key: private_view.public(),
                public_spend_key: public_spend,
                transfer_proof,
                expected: crate::monero::Amount::from_piconero(state4.xmr_amount().as_pico()),
            },
            on_confirmation,
        )
        .await?;
    Ok(())
}

/// Publish the Bitcoin cancel transaction and persist that it is live.
pub async fn publish_cancel(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state6: &machine::bob::State6,
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
) -> Result<bitcoin::Txid> {
    let (txid, _subscription) = state6.submit_tx_cancel(bitcoin_wallet).await?;

    store
        .store_bob(swap_id, &db::bob::Bob::BtcCancelPublished(state6.clone()))
        .await?;

    Ok(txid)
}

/// Publish the best available Bitcoin refund and persist that it is live.
pub async fn publish_refund(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state6: &machine::bob::State6,
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
) -> Result<(bitcoin::Txid, bitcoin_wallet::Subscription, machine::bob::RefundType)> {
    let (transaction, refund_type) = state6.construct_best_bitcoin_refund_tx()?;
    let (txid, subscription) = bitcoin_wallet
        .ensure_broadcasted(transaction, "refund")
        .await?;

    // A seller that asked for an anti-spam deposit only signed a partial refund; record it
    // as such so a resume continues with the deposit (upstream `BtcPartialRefundPublished`).
    let published = match refund_type {
        machine::bob::RefundType::Full => db::bob::Bob::BtcRefundPublished(state6.clone()),
        machine::bob::RefundType::Partial { .. } => {
            db::bob::Bob::BtcPartialRefundPublished(state6.clone())
        }
    };
    store.store_bob(swap_id, &published).await?;

    Ok((txid, subscription, refund_type))
}

/// How a Bitcoin refund ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefundOutcome {
    /// The whole lock came back, less fees: a full refund or the seller's early refund.
    Refunded(bitcoin::Txid),
    /// A partial refund, and then the deposit came back too (reclaim, or the seller's
    /// mercy after a withhold). The txid is the transaction that returned the deposit.
    DepositReturned(bitcoin::Txid),
    /// A partial refund; the seller burnt the deposit with its withhold transaction.
    DepositWithheld {
        withhold_txid: bitcoin::Txid,
        deposit: bitcoin::Amount,
        lock_amount: bitcoin::Amount,
    },
}

/// After a refund has confirmed: a full refund is done; a partial refund goes on to
/// reclaim the deposit.
pub(crate) async fn finish_refund(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state6: &machine::bob::State6,
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
    refund_type: machine::bob::RefundType,
    txid: bitcoin::Txid,
    on_progress: &impl Fn(&str),
) -> Result<RefundOutcome> {
    match refund_type {
        machine::bob::RefundType::Full => {
            persist_refund_done(store, swap_id, state6.clone()).await?;
            Ok(RefundOutcome::Refunded(txid))
        }
        machine::bob::RefundType::Partial { btc_amnesty_amount, .. } => {
            store
                .store_bob(swap_id, &db::bob::Bob::BtcPartiallyRefunded(state6.clone()))
                .await?;
            on_progress(&format!(
                "Partial Bitcoin refund {txid} confirmed. The seller's deposit of {} sats stays locked for a few more blocks; the app then claims it back.",
                btc_amnesty_amount.to_sat()
            ));
            reclaim_deposit(store, swap_id, state6, bitcoin_wallet, on_progress).await
        }
    }
}

/// The chain state of a partial refund's deposit, read every round of [`reclaim_deposit`].
#[derive(Debug, Clone, Copy)]
struct DepositStatuses {
    partial_refund: bitcoin_wallet::ScriptStatus,
    reclaim: bitcoin_wallet::ScriptStatus,
    withhold: bitcoin_wallet::ScriptStatus,
    mercy: bitcoin_wallet::ScriptStatus,
}

/// What [`reclaim_deposit`] does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepositStep {
    /// The reclaim confirmed: the deposit is back.
    Reclaimed,
    /// The seller withheld and then returned the deposit (mercy), confirmed.
    MercyConfirmed,
    /// The seller's withhold confirmed and no mercy is in sight: the deposit is gone.
    Withheld,
    /// Withhold confirmed, mercy in the mempool: wait for it.
    WaitForMercy,
    /// The seller's withhold is in the mempool: a reclaim would only conflict; wait.
    WaitForWithhold,
    /// Our reclaim is in the mempool: wait for it.
    WaitForReclaim,
    /// The deposit timelock has passed and nothing spends the deposit yet: publish.
    PublishReclaim,
    /// The partial refund needs `blocks_left` more confirmations.
    WaitForTimelock { blocks_left: u32 },
}

/// Upstream `WaitingForReclaimTimelockExpiration` / `ReclaimTimelockExpired` /
/// `BtcReclaimPublished` / `BtcWithholdPublished` / `BtcWithheld` / `BtcMercyPublished`
/// as one decision. Unlike upstream, a confirmed withhold also ends a reclaim wait.
fn deposit_step(statuses: &DepositStatuses, remaining_refund_timelock: u32) -> DepositStep {
    use bitcoin_wallet::ScriptStatus::Confirmed;
    if matches!(statuses.reclaim, Confirmed(_)) {
        return DepositStep::Reclaimed;
    }
    if matches!(statuses.mercy, Confirmed(_)) {
        return DepositStep::MercyConfirmed;
    }
    if matches!(statuses.withhold, Confirmed(_)) {
        return if statuses.mercy.has_been_seen() {
            DepositStep::WaitForMercy
        } else {
            DepositStep::Withheld
        };
    }
    if statuses.withhold.has_been_seen() {
        return DepositStep::WaitForWithhold;
    }
    if statuses.reclaim.has_been_seen() {
        return DepositStep::WaitForReclaim;
    }
    if statuses.partial_refund.is_confirmed_with(remaining_refund_timelock) {
        return DepositStep::PublishReclaim;
    }
    DepositStep::WaitForTimelock {
        blocks_left: statuses
            .partial_refund
            .blocks_left_until(remaining_refund_timelock),
    }
}

/// After a confirmed partial refund, claim the seller's deposit back once its timelock
/// has passed, unless the seller burns it first (audit H-1; upstream Bob's reclaim,
/// withhold and mercy states). Each stage is persisted so a resume continues here.
pub async fn reclaim_deposit(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state6: &machine::bob::State6,
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
    on_progress: &impl Fn(&str),
) -> Result<RefundOutcome> {
    let deposit = state6.btc_amnesty_amount.unwrap_or(bitcoin::Amount::ZERO);
    let lock_amount = state6.tx_lock.lock_amount();
    let remaining = u32::from(
        state6
            .remaining_refund_timelock
            .context("the swap has no deposit timelock, so there is no deposit to reclaim")?,
    );
    let tx_partial_refund = state6.construct_tx_partial_refund()?;
    let tx_reclaim = state6.construct_tx_amnesty()?;
    let tx_withhold = state6.construct_tx_withhold()?;
    let tx_mercy = state6.construct_tx_mercy()?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(7 * 24 * 60 * 60);
    let mut saved: Option<&'static str> = None;
    let mut reported_blocks_left: Option<u32> = None;
    let mut reported_withhold = false;

    loop {
        let read = async {
            Ok::<_, anyhow::Error>(DepositStatuses {
                partial_refund: bitcoin_wallet.status_of_script(&tx_partial_refund).await?,
                reclaim: bitcoin_wallet.status_of_script(&tx_reclaim).await?,
                withhold: bitcoin_wallet.status_of_script(&tx_withhold).await?,
                mercy: bitcoin_wallet.status_of_script(&tx_mercy).await?,
            })
        };
        match read.await {
            Err(error) => {
                tracing::warn!(%error, "could not read the deposit's status; retrying");
            }
            Ok(statuses) => {
                let step = deposit_step(&statuses, remaining);
                // Persist each stage once, as upstream's states, so a resume lands here.
                let stage = match step {
                    DepositStep::WaitForTimelock { .. } => Some((
                        "waiting",
                        db::bob::Bob::WaitingForReclaimTimelockExpiration(state6.clone()),
                    )),
                    DepositStep::PublishReclaim => Some((
                        "expired",
                        db::bob::Bob::ReclaimTimelockExpired(state6.clone()),
                    )),
                    DepositStep::WaitForReclaim => Some((
                        "reclaim",
                        db::bob::Bob::BtcReclaimPublished(state6.clone()),
                    )),
                    DepositStep::WaitForWithhold => Some((
                        "withhold",
                        db::bob::Bob::BtcWithholdPublished(state6.clone()),
                    )),
                    DepositStep::WaitForMercy => Some((
                        "mercy",
                        db::bob::Bob::BtcMercyPublished(state6.clone()),
                    )),
                    _ => None,
                };
                if let Some((name, state)) = stage {
                    if saved != Some(name) {
                        store.store_bob(swap_id, &state).await?;
                        saved = Some(name);
                    }
                }

                match step {
                    DepositStep::Reclaimed => {
                        store
                            .store_bob(
                                swap_id,
                                &db::bob::Bob::Done(db::bob::BobEndState::BtcReclaimConfirmed(
                                    Box::new(state6.clone()),
                                )),
                            )
                            .await?;
                        tracing::info!(txid = %tx_reclaim.txid(), "V5_BOB_DEPOSIT_RECLAIMED");
                        on_progress(&format!(
                            "The deposit of {} sats is back (transaction {}).",
                            deposit.to_sat(),
                            tx_reclaim.txid()
                        ));
                        return Ok(RefundOutcome::DepositReturned(tx_reclaim.txid()));
                    }
                    DepositStep::MercyConfirmed => {
                        store
                            .store_bob(
                                swap_id,
                                &db::bob::Bob::Done(db::bob::BobEndState::BtcMercyConfirmed(
                                    Box::new(state6.clone()),
                                )),
                            )
                            .await?;
                        tracing::info!(txid = %tx_mercy.txid(), "V5_BOB_DEPOSIT_MERCY");
                        on_progress("The seller returned the withheld deposit.");
                        return Ok(RefundOutcome::DepositReturned(tx_mercy.txid()));
                    }
                    DepositStep::Withheld => {
                        store
                            .store_bob(swap_id, &db::bob::Bob::BtcWithheld(state6.clone()))
                            .await?;
                        tracing::warn!(txid = %tx_withhold.txid(), deposit = deposit.to_sat(), "V5_BOB_DEPOSIT_WITHHELD");
                        return Ok(RefundOutcome::DepositWithheld {
                            withhold_txid: tx_withhold.txid(),
                            deposit,
                            lock_amount,
                        });
                    }
                    DepositStep::PublishReclaim => {
                        let transaction = state6.signed_amnesty_transaction()?;
                        match bitcoin_wallet.ensure_broadcasted(transaction, "reclaim").await {
                            Ok((txid, _)) => {
                                store
                                    .store_bob(
                                        swap_id,
                                        &db::bob::Bob::BtcReclaimPublished(state6.clone()),
                                    )
                                    .await?;
                                saved = Some("reclaim");
                                on_progress(&format!(
                                    "Deposit reclaim {txid} broadcast; waiting for confirmation."
                                ));
                            }
                            Err(error) => {
                                tracing::warn!(%error, "deposit reclaim broadcast failed; retrying");
                            }
                        }
                    }
                    DepositStep::WaitForWithhold => {
                        if !reported_withhold {
                            reported_withhold = true;
                            on_progress("The seller is withholding the deposit; waiting to see whether that confirms.");
                        }
                    }
                    DepositStep::WaitForTimelock { blocks_left } => {
                        if reported_blocks_left != Some(blocks_left) {
                            reported_blocks_left = Some(blocks_left);
                            on_progress(&format!(
                                "Waiting {blocks_left} more block(s) before the deposit can be claimed back."
                            ));
                        }
                    }
                    DepositStep::WaitForReclaim | DepositStep::WaitForMercy => {}
                }
            }
        }

        if tokio::time::Instant::now() >= deadline {
            bail!("timed out waiting to reclaim the Bitcoin deposit");
        }
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

/// Publish the refund and wait for one confirmation before reporting it as
/// complete. A published transaction can still sit in the mempool; the UI must
/// not tell the user they were refunded until the chain proves it.
async fn publish_and_confirm_refund(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state6: &machine::bob::State6,
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
    on_progress: &impl Fn(&str),
    on_refund_broadcast: &impl Fn(bitcoin::Txid),
) -> Result<RefundOutcome> {
    let (txid, subscription, refund_type) =
        publish_refund(store, swap_id, state6, bitcoin_wallet).await?;
    on_refund_broadcast(txid);
    on_progress(&format!(
        "Bitcoin refund transaction broadcast: {txid}; waiting for confirmation."
    ));
    subscription.wait_until_confirmed_with(1u32).await?;
    finish_refund(store, swap_id, state6, bitcoin_wallet, refund_type, txid, on_progress).await
}

/// Wait for a persisted refund transaction to confirm before reporting a
/// completed refund on resume. Published is not the same as confirmed.
pub async fn wait_for_refund_confirmation(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state6: &machine::bob::State6,
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
    on_refund_broadcast: &impl Fn(bitcoin::Txid),
    on_progress: &impl Fn(&str),
) -> Result<RefundOutcome> {
    let (transaction, refund_type) = state6.construct_best_bitcoin_refund_tx()?;
    let (txid, subscription) = bitcoin_wallet
        .ensure_broadcasted(transaction, "refund")
        .await?;
    on_refund_broadcast(txid);
    subscription.wait_until_confirmed_with(1u32).await?;
    finish_refund(store, swap_id, state6, bitcoin_wallet, refund_type, txid, on_progress).await
}

/// What happened to Alice's early refund.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarlyRefundOutcome {
    /// It confirmed: the Bitcoin is back and the swap is done.
    Confirmed(bitcoin::Txid),
    /// The cancel timelock expired first. The swap is stored as `BtcCancelled`, so the
    /// normal cancel/refund path takes over (audit 2026-11-10, H-3).
    CancelTimelockExpired,
}

/// Wait for Alice's pre-signed early refund to confirm, but not past the cancel timelock.
///
/// Bob never broadcasts this transaction; Alice does. While it is unconfirmed the lock output
/// may still be unspent, and only Bob's own cancel/refund can then save the Bitcoin before the
/// maker can punish. If the early refund confirms after all, the cancel cannot spend the lock
/// and the next resume finds the early refund.
pub async fn wait_for_early_refund_confirmation(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state6: &machine::bob::State6,
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
) -> Result<EarlyRefundOutcome> {
    let transaction = state6.construct_tx_early_refund();
    let txid = transaction.txid();
    let subscription = bitcoin_wallet.subscribe_to(Box::new(transaction)).await;

    let confirmed = race_early_refund(
        subscription.wait_until_confirmed_with(1u32),
        wait_until_cancel_timelock_expired(
            || state6.expired_timelock(bitcoin_wallet),
            std::time::Duration::from_secs(30),
        ),
    )
    .await?;

    if !confirmed {
        tracing::warn!(%txid, "early refund not confirmed by the cancel timelock; falling back to cancel/refund");
        // Nothing has been cancelled yet: record the timelock, as upstream does. The
        // cancel/refund loop still watches this early refund (see `early_refund_check`).
        store
            .store_bob(swap_id, &db::bob::Bob::CancelTimelockExpired(state6.clone()))
            .await?;
        return Ok(EarlyRefundOutcome::CancelTimelockExpired);
    }

    store
        .store_bob(
            swap_id,
            &db::bob::Bob::Done(db::bob::BobEndState::BtcEarlyRefunded(Box::new(
                state6.clone(),
            ))),
        )
        .await?;

    Ok(EarlyRefundOutcome::Confirmed(txid))
}

/// `true` when `confirmed` finishes first, `false` when `cancel_expired` does. A confirmation
/// that is ready at the same moment wins.
async fn race_early_refund(
    confirmed: impl std::future::Future<Output = Result<()>>,
    cancel_expired: impl std::future::Future<Output = Result<()>>,
) -> Result<bool> {
    tokio::select! {
        biased;
        result = confirmed => result.map(|()| true),
        result = cancel_expired => result.map(|()| false),
    }
}

/// Polls the timelock every `interval` until the cancel timelock has expired. Read failures
/// are retried (see [`expired_timelock_retrying`]).
async fn wait_until_cancel_timelock_expired<F, Fut>(
    mut read: F,
    interval: std::time::Duration,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<crate::protocol_v5::core::bitcoin::ExpiredTimelocks>>,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(7 * 24 * 60 * 60);
    loop {
        let timelock = expired_timelock_retrying(&mut read, deadline).await?;
        if timelock.cancel_timelock_expired() {
            return Ok(());
        }
        tokio::time::sleep(interval).await;
    }
}

/// Persist the terminal state only after the refund has confirmed.
pub async fn persist_refund_done(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state6: machine::bob::State6,
) -> Result<()> {
    store
        .store_bob(
            swap_id,
            &db::bob::Bob::Done(db::bob::BobEndState::BtcRefunded(Box::new(state6))),
        )
        .await
}

/// Persist the terminal punished state after the refund window is lost.
pub async fn persist_punished(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state6: machine::bob::State6,
) -> Result<()> {
    let tx_lock_id = state6.tx_lock_id();
    store
        .store_bob(
            swap_id,
            &db::bob::Bob::BtcPunished {
                state: state6,
                tx_lock_id,
            },
        )
        .await
}

/// Publish cancel, wait for the refund timelock, then publish and confirm the refund.
///
/// `on_cancel_broadcast` fires only after the cancel transaction was accepted
/// for broadcast. The legacy client notified the UI at that exact point; v5
/// previously notified before the first attempt, which made the UI claim a
/// cancel transaction that only existed locally.
pub async fn run_cancel_and_refund(
    store: &dyn ProtocolV5Store,
    swap_id: Uuid,
    state6: machine::bob::State6,
    bitcoin_wallet: &dyn bitcoin_wallet::BitcoinWallet,
    on_progress: impl Fn(&str),
    on_cancel_broadcast: impl Fn(bitcoin::Txid),
    on_refund_broadcast: impl Fn(bitcoin::Txid),
) -> Result<RefundOutcome> {
    // Fail before waiting if the persisted setup cannot produce a refund.
    state6.construct_best_bitcoin_refund_tx()?;

    // Testnet's cancel timelock is 42 blocks (≈7 h at the 10-min target), and
    // testnet4 stalls can stretch that well past the old 6 h cap. Keep a
    // generous safety bound; the punish timelock still guards the path.
    // Testnet4 can take over an hour per block; 42 cancel blocks can exceed
    // 24 h. The punish timelock is the real bound, so keep a week of slack.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(7 * 24 * 60 * 60);
    let mut cancel_published = false;
    let mut cancel_attempts = 0u64;
    let tx_early_refund = state6.construct_tx_early_refund();
    let early_refund_txid = tx_early_refund.txid();
    let mut early_refund_pending_reported = false;
    loop {
        // The seller's early refund spends the same lock output as our cancel. Once it
        // confirms the Bitcoin is back and a cancel can never be valid ("inputs spent",
        // which is retried), so without this check the loop never ended (audit H-3,
        // upstream `CancelTimelockExpired` / `BtcCancelPublished`).
        let early_refund = match bitcoin_wallet.status_of_script(&tx_early_refund).await {
            Ok(status) => early_refund_check(&status),
            Err(error) => {
                tracing::debug!(%error, %early_refund_txid, "could not read the early refund status; carrying on with cancel/refund");
                EarlyRefundCheck::Absent
            }
        };
        match early_refund {
            EarlyRefundCheck::Refunded => {
                tracing::info!(%early_refund_txid, "V5_BOB_EARLY_REFUND_CONFIRMED");
                store
                    .store_bob(
                        swap_id,
                        &db::bob::Bob::Done(db::bob::BobEndState::BtcEarlyRefunded(Box::new(
                            state6.clone(),
                        ))),
                    )
                    .await?;
                on_progress(&format!(
                    "The seller's early refund {early_refund_txid} confirmed; your Bitcoin is back."
                ));
                return Ok(RefundOutcome::Refunded(early_refund_txid));
            }
            EarlyRefundCheck::Pending => {
                // Publishing a cancel now would only conflict with it in the mempool.
                if !early_refund_pending_reported {
                    early_refund_pending_reported = true;
                    on_progress(&format!(
                        "The seller's early refund {early_refund_txid} is waiting to confirm; waiting for it instead of cancelling."
                    ));
                }
                if tokio::time::Instant::now() >= deadline {
                    bail!("timed out waiting for the Bitcoin refund timelock");
                }
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                continue;
            }
            EarlyRefundCheck::Absent => {}
        }

        // The cancel can be rejected with `non-BIP68-final` when broadcast
        // before the timelock expires (swap c365206b). That is not fatal:
        // retry the broadcast instead of abandoning the refund path.
        if !cancel_published {
            match publish_cancel(store, swap_id, &state6, bitcoin_wallet).await {
                Ok(txid) => {
                    cancel_published = true;
                    on_cancel_broadcast(txid);
                    on_progress(
                        "Bitcoin refund timelock reached; cancel transaction broadcast; waiting for confirmation.",
                    );
                }
                Err(error) if cancel_broadcast_is_retryable(&error) => {
                    cancel_attempts += 1;
                    tracing::info!(
                        %error,
                        "cancel broadcast rejected or failed; will retry until the timelock makes it final"
                    );
                    if cancel_attempts == 1 || cancel_attempts % 5 == 0 {
                        on_progress(&format!(
                            "Bitcoin refund timelock not mature yet ({}); retrying in 60s.",
                            cancel_retry_reason(&error)
                        ));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        // A failed read is a network problem, not the end of the swap: before, one Electrum
        // error here ended the client with Bitcoin locked and nothing watching (audit H-2).
        let timelock =
            expired_timelock_retrying(|| state6.expired_timelock(bitcoin_wallet), deadline).await?;
        match timelock {
            // Once the cancel timelock has expired, Bob's refund is valid as
            // soon as the cancel transaction is confirmed. Retry broadcast
            // errors because the cancel may still be in the mempool.
            crate::protocol_v5::core::bitcoin::ExpiredTimelocks::Cancel { .. } => {
                match publish_and_confirm_refund(
                    store,
                    swap_id,
                    &state6,
                    bitcoin_wallet,
                    &on_progress,
                    &on_refund_broadcast,
                )
                .await
                {
                    Ok(outcome) => return Ok(outcome),
                    Err(error) => tracing::info!(
                        %error,
                        "waiting for the Bitcoin cancel to confirm before publishing the refund"
                    ),
                }
            }
            // A partial refund is already on chain (we may have missed it while offline):
            // publishing it again is a no-op, and its confirmation leads to the deposit.
            crate::protocol_v5::core::bitcoin::ExpiredTimelocks::WaitingForRemainingRefund { .. }
            | crate::protocol_v5::core::bitcoin::ExpiredTimelocks::RemainingRefund => {
                match publish_and_confirm_refund(
                    store,
                    swap_id,
                    &state6,
                    bitcoin_wallet,
                    &on_progress,
                    &on_refund_broadcast,
                )
                .await
                {
                    Ok(outcome) => return Ok(outcome),
                    Err(error) => tracing::info!(
                        %error,
                        "refund broadcast or confirmation failed; retrying"
                    ),
                }
            }
            // The punish timelock passing does not mean the maker punished: until a punish
            // transaction confirms, the refund still spends the cancel output. Before, the
            // client recorded "punished" here and gave up (audit 2026-11-10, H-5).
            crate::protocol_v5::core::bitcoin::ExpiredTimelocks::Punish => {
                let tx_punish = state6.construct_tx_punish()?;
                let punish_txid = tx_punish.txid();
                match punish_window_action(&bitcoin_wallet.status_of_script(&tx_punish).await) {
                    PunishWindowAction::RecordPunished => {
                        persist_punished(store, swap_id, state6.clone()).await?;
                        return Err(Punished(punish_txid).into());
                    }
                    PunishWindowAction::TryRefund => {
                        on_progress(
                            "Bitcoin punish timelock has passed but the seller has not punished; trying the refund.",
                        );
                        match publish_and_confirm_refund(
                            store,
                            swap_id,
                            &state6,
                            bitcoin_wallet,
                            &on_progress,
                            &on_refund_broadcast,
                        )
                        .await
                        {
                            Ok(outcome) => return Ok(outcome),
                            Err(error) => tracing::warn!(
                                %error,
                                "refund after the punish timelock failed; checking the punish transaction again"
                            ),
                        }
                    }
                }
            }
            _ => {}
        }

        if tokio::time::Instant::now() >= deadline {
            bail!("timed out waiting for the Bitcoin refund timelock");
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

/// Reads the timelock until a read succeeds or `deadline` passes, waiting a little longer
/// after each failure (15 s, 30 s, ... up to 60 s).
async fn expired_timelock_retrying<F, Fut>(
    mut read: F,
    deadline: tokio::time::Instant,
) -> Result<crate::protocol_v5::core::bitcoin::ExpiredTimelocks>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<crate::protocol_v5::core::bitcoin::ExpiredTimelocks>>,
{
    let mut failures = 0u32;
    loop {
        match read().await {
            Ok(timelock) => return Ok(timelock),
            Err(error) => {
                failures += 1;
                tracing::warn!(%error, failures, "could not read the Bitcoin timelock; retrying");
                if tokio::time::Instant::now() >= deadline {
                    return Err(error.context("timed out reading the Bitcoin refund timelock"));
                }
                let wait = std::time::Duration::from_secs((15 * failures as u64).min(60));
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// The maker's punish transaction is confirmed: the Bitcoin is gone. Callers report this as
/// a punished swap, not as an error to retry.
#[derive(Debug, thiserror::Error)]
#[error("the swap was punished: the seller's punish transaction {0} is confirmed")]
pub struct Punished(pub bitcoin::Txid);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PunishWindowAction {
    RecordPunished,
    TryRefund,
}

/// After the punish timelock, only a confirmed punish ends the swap. Unseen, in the mempool,
/// or unknown (a failed read) all leave the refund worth trying.
/// What the cancel/refund loop does about the seller's early refund (audit H-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EarlyRefundCheck {
    /// Confirmed: the Bitcoin is back; the swap ends as early-refunded.
    Refunded,
    /// In the mempool: wait for it rather than publish a conflicting cancel.
    Pending,
    /// Not seen: carry on with cancel/refund.
    Absent,
}

fn early_refund_check(status: &bitcoin_wallet::ScriptStatus) -> EarlyRefundCheck {
    match status {
        bitcoin_wallet::ScriptStatus::Confirmed(_) => EarlyRefundCheck::Refunded,
        bitcoin_wallet::ScriptStatus::InMempool => EarlyRefundCheck::Pending,
        bitcoin_wallet::ScriptStatus::Unseen | bitcoin_wallet::ScriptStatus::Retrying => {
            EarlyRefundCheck::Absent
        }
    }
}

fn punish_window_action(
    status: &Result<bitcoin_wallet::ScriptStatus>,
) -> PunishWindowAction {
    match status {
        Ok(bitcoin_wallet::ScriptStatus::Confirmed(_)) => PunishWindowAction::RecordPunished,
        _ => PunishWindowAction::TryRefund,
    }
}

/// Reasons bitcoind gives for a transaction that can never become valid.
const PERMANENT_REJECTIONS: &[&str] = &[
    "mandatory-script-verify-flag-failed",
    "bad-txns-in-belowout",
    "bad-txns-inputs-duplicate",
    "bad-txns-vout-negative",
    "bad-txns-vout-toolarge",
    "bad-txns-txouttotal-toolarge",
    "bad-txns-oversize",
    "tx-size-small",
];

/// Whether `text` carries the bitcoind policy-reject code -26 as a code, not as
/// part of a date or an id (`2026-10-26`, `…ccdc-26ab`). The `-26` must follow a
/// separator, not a letter or digit, must not run into more digits, and must sit
/// next to the word "code" or "error".
fn mentions_rpc_code_26(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(found) = text[from..].find("-26") {
        let start = from + found;
        let end = start + 3;
        from = end;

        let before_ok = start == 0
            || matches!(bytes[start - 1], b' ' | b':' | b'=' | b'(' | b'"' | b'\'' | b'[');
        let after_ok = end >= bytes.len() || !(bytes[end] as char).is_ascii_alphanumeric();
        if !before_ok || !after_ok {
            continue;
        }
        let window_start = start.saturating_sub(24);
        let context = text[window_start..start].to_ascii_lowercase();
        if context.contains("code") || context.contains("error") {
            return true;
        }
    }
    false
}

/// Classify a cancel-broadcast failure: retryable (the transaction may become
/// acceptable later, e.g. `non-BIP68-final` before the timelock expires) or
/// terminal. Swap c365206b treated the retryable case as terminal and surfaced
/// the whole refund as failed.
///
/// Matches on the rendered error chain instead of downcasting: the failure can
/// arrive as a raw Electrum RPC error, a `MultiError` over several servers, or
/// a wrapped Dojo timeout, and string matching covers all three shapes.
fn cancel_broadcast_is_retryable(error: &anyhow::Error) -> bool {
    let rendered = error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join("; ");

    // Rejections the network will never accept, however long we wait. They can
    // arrive wrapped in a -26 reject, so they are checked first.
    if PERMANENT_REJECTIONS.iter().any(|reason| rendered.contains(reason)) {
        return false;
    }

    // Consensus rejections that time/resolution can fix.
    if rendered.contains("non-BIP68-final")
        || rendered.contains("txn-mempool-conflict")
        || rendered.contains("missing-inputs")
        || rendered.contains("bad-txns-inputs-missingorspent")
    {
        return true;
    }

    // Infrastructure failures (Dojo timeout, connection refused, ...) are
    // retryable too. Anything advertising code -26/-1 textually lands here.
    let is_rpc_reject = mentions_rpc_code_26(&rendered) || rendered.contains("rejected by network rules");
    let is_transport = rendered.contains("timed out")
        || rendered.contains("Connection refused")
        || rendered.contains("Failed to broadcast");
    is_rpc_reject || is_transport
}

/// Short, stable reason for the swap UI; the full error stays in the tracing
/// log for diagnosis.
fn cancel_retry_reason(error: &anyhow::Error) -> &'static str {
    let rendered = error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    if rendered.contains("non-BIP68-final") {
        "non-BIP68-final, timelock not mature"
    } else if rendered.contains("timed out") {
        "broadcast timed out"
    } else {
        "broadcast unavailable"
    }
}

#[cfg(test)]
mod tests {
    use super::{cancel_broadcast_is_retryable, redeem_wallet_name};

    #[test]
    fn redeem_wallet_name_is_keyed_by_restore_height() {
        let swap_id = uuid::Uuid::nil();
        assert_eq!(
            redeem_wallet_name(swap_id, 2_245_041),
            format!("{swap_id}-redeem-wallet-2245041")
        );
        // A stale recorded height and the real lock height must never share a
        // wallet file, or the retry reopens the stale scan.
        assert_ne!(
            redeem_wallet_name(swap_id, 2_240_000),
            redeem_wallet_name(swap_id, 2_245_041)
        );
    }

    #[test]
    fn non_bip68_final_cancel_rejection_is_retryable() {
        // Exactly the failure swap c365206b hit: Electrum rejecting the cancel
        // because the timelock had not expired yet.
        let error = anyhow::anyhow!(
            "Bitcoin cancel transaction 11ad7733 failed to broadcast on all 1 servers: 1 errors occurred 1: Electrum server error: {{\"code\":1,\"message\":\"the transaction was rejected by network rules.\\n\\nnon-BIP68-final\"}}"
        );
        assert!(cancel_broadcast_is_retryable(&error));
    }

    #[test]
    fn dojo_timeout_without_rpc_code_is_retryable() {
        assert!(cancel_broadcast_is_retryable(&anyhow::anyhow!(
            "Dojo broadcast timed out after 15 seconds"
        )));
    }

    #[test]
    fn mempool_conflict_rejection_is_retryable() {
        let error = anyhow::anyhow!(
            "Electrum server error: {{\"code\":-26,\"message\":\"txn-mempool-conflict\"}}"
        );
        assert!(cancel_broadcast_is_retryable(&error));
    }

    // The classifier used `rendered.contains("-26")`, which matches dates and id
    // fragments, so a terminal failure containing one retried silently for days.
    #[test]
    fn a_date_or_id_containing_26_is_not_an_rpc_reject() {
        assert!(!cancel_broadcast_is_retryable(&anyhow::anyhow!(
            "swap 2026-10-26 could not build the cancel transaction: invalid witness"
        )));
        assert!(!cancel_broadcast_is_retryable(&anyhow::anyhow!(
            "swap e9cc1124ccdc-26ab could not build the cancel transaction"
        )));
    }

    // A script the network will never accept is terminal even when it arrives
    // wrapped in a -26 reject.
    #[test]
    fn a_permanent_script_rejection_is_terminal() {
        let error = anyhow::anyhow!(
            "Electrum server error: {{\"code\":-26,\"message\":\"mandatory-script-verify-flag-failed (Signature must be zero for failed CHECK(MULTI)SIG operation)\"}}"
        );
        assert!(!cancel_broadcast_is_retryable(&error));
        let error = anyhow::anyhow!("RPC error -26: bad-txns-in-belowout");
        assert!(!cancel_broadcast_is_retryable(&error));
    }

    #[test]
    fn a_real_rpc_26_without_a_permanent_reason_is_still_retryable() {
        assert!(cancel_broadcast_is_retryable(&anyhow::anyhow!("RPC error -26: txn-mempool-conflict")));
        assert!(cancel_broadcast_is_retryable(&anyhow::anyhow!("error code: -26 (policy)")));
        assert!(cancel_broadcast_is_retryable(&anyhow::anyhow!(
            "the transaction was rejected by network rules"
        )));
    }

    #[test]
    fn signature_failure_is_terminal() {
        // A malformed/unsigned cancel will never become valid; retrying is
        // pointless and the error must surface.
        let error = anyhow::anyhow!(
            "Failed to complete Bitcoin cancel transaction: missing witness signature"
        );
        assert!(!cancel_broadcast_is_retryable(&error));
    }
}

#[cfg(test)]
mod sweep_mined_tests {
    use super::wait_until_mined;
    use std::time::Duration;

    #[tokio::test]
    async fn returns_the_height_once_the_sweep_is_mined() {
        let mut calls = 0;
        let height = wait_until_mined(
            || {
                calls += 1;
                let result = if calls < 3 { Ok(None) } else { Ok(Some(42)) };
                async move { result }
            },
            Duration::from_millis(1),
            10,
        )
        .await
        .unwrap();
        assert_eq!(height, 42);
    }

    #[tokio::test]
    async fn a_sweep_that_never_mines_is_an_error_not_done() {
        let result = wait_until_mined(
            || async { Ok(None) },
            Duration::from_millis(1),
            3,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn query_errors_are_retried() {
        let mut calls = 0;
        let height = wait_until_mined(
            || {
                calls += 1;
                let result = if calls == 1 { Err(anyhow::anyhow!("daemon timeout")) } else { Ok(Some(7)) };
                async move { result }
            },
            Duration::from_millis(1),
            5,
        )
        .await
        .unwrap();
        assert_eq!(height, 7);
    }
}

#[cfg(test)]
mod stored_sweep_tests {
    use super::*;
    use crate::monero::ng::TxStatus;

    #[test]
    fn with_nothing_stored_a_new_sweep_is_built() {
        assert_eq!(plan_stored_sweep(false, None), StoredSweepPlan::BuildNew);
        assert_eq!(
            plan_stored_sweep(false, Some(TxStatus::Mined(5))),
            StoredSweepPlan::BuildNew
        );
    }

    // The crash window this closes: the daemon accepted the sweep, then the app stopped.
    // Building a second sweep would be refused (same output) and show an error although the
    // Monero was swept.
    #[test]
    fn a_stored_sweep_the_daemon_has_is_never_rebuilt() {
        assert_eq!(
            plan_stored_sweep(true, Some(TxStatus::Mined(2_246_506))),
            StoredSweepPlan::Finish
        );
        assert_eq!(
            plan_stored_sweep(true, Some(TxStatus::InPool)),
            StoredSweepPlan::Wait
        );
    }

    #[test]
    fn a_stored_sweep_the_daemon_does_not_know_is_sent_again() {
        assert_eq!(
            plan_stored_sweep(true, Some(TxStatus::Unknown)),
            StoredSweepPlan::Rebroadcast
        );
        assert_eq!(plan_stored_sweep(true, None), StoredSweepPlan::Rebroadcast);
    }
}

#[cfg(test)]
mod witness_gate_tests {
    use super::*;
    use crate::monero::witness::{LockWitness, Witnesses};
    use async_trait::async_trait;
    use std::sync::Arc;

    struct Answer(Result<bool, &'static str>);

    #[async_trait]
    impl LockWitness for Answer {
        fn name(&self) -> String {
            "witness".into()
        }

        async fn lock_is_valid(&self, _: &str, _: &[u8; 32], _: &[u8], _: u64) -> Result<bool> {
            self.0.map_err(|reason| anyhow::anyhow!(reason))
        }

        async fn confirmations(&self, _: &str) -> Result<Option<u64>> {
            Ok(Some(10))
        }
    }

    async fn gate(witnesses: Option<Witnesses>) -> Result<bool> {
        witnesses_agree(witnesses.as_ref(), false, "tx", &[0; 32], &[0; 32], 1).await
    }

    async fn required_gate(witnesses: Option<Witnesses>) -> Result<bool> {
        witnesses_agree(witnesses.as_ref(), true, "tx", &[0; 32], &[0; 32], 1).await
    }

    fn set(answers: Vec<Result<bool, &'static str>>) -> Option<Witnesses> {
        Some(Witnesses::new(
            answers.into_iter().map(|a| Arc::new(Answer(a)) as Arc<dyn LockWitness>).collect(),
        ))
    }

    #[tokio::test]
    async fn without_witnesses_the_gate_is_open() {
        assert!(gate(None).await.unwrap());
        assert!(gate(set(vec![])).await.unwrap());
    }

    #[tokio::test]
    async fn witnesses_that_all_agree_open_the_gate() {
        assert!(gate(set(vec![Ok(true), Ok(true)])).await.unwrap());
    }

    // The lock is reported as not verified, so the signature is not sent.
    #[tokio::test]
    async fn one_disagreeing_witness_closes_the_gate() {
        assert!(!gate(set(vec![Ok(true), Ok(false)])).await.unwrap());
    }

    #[tokio::test]
    async fn an_unreachable_witness_is_an_error_not_a_pass() {
        let error = gate(set(vec![Ok(true), Err("connection refused")])).await.unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("not sending the signature"), "{message}");
        assert!(message.contains("connection refused"), "{message}");
    }

    // Too few witness nodes were available: the check must not be skipped.
    #[tokio::test]
    async fn required_witnesses_that_are_missing_close_the_gate() {
        for witnesses in [None, set(vec![])] {
            let error = required_gate(witnesses).await.unwrap_err();
            assert!(format!("{error:#}").contains("required"), "{error:#}");
        }
    }

    #[tokio::test]
    async fn required_witnesses_that_agree_open_the_gate() {
        assert!(required_gate(set(vec![Ok(true), Ok(true)])).await.unwrap());
    }

    #[test]
    fn the_sweep_wait_logs_once_a_minute_and_not_at_the_start() {
        let due: Vec<u32> = (0..20).filter(|poll| super::sweep_wait_log_due(*poll)).collect();
        assert_eq!(due, vec![6, 12, 18]);
    }
}

#[cfg(test)]
mod timelock_retry_tests {
    use super::expired_timelock_retrying;
    use crate::protocol_v5::core::bitcoin::ExpiredTimelocks;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use tokio::time::Instant;

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(7 * 24 * 60 * 60)
    }

    #[tokio::test(start_paused = true)]
    async fn a_successful_read_is_returned_at_once() {
        let calls = AtomicU32::new(0);
        let result = expired_timelock_retrying(
            || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(ExpiredTimelocks::None { blocks_left: 3 })
            },
            far(),
        )
        .await
        .unwrap();
        assert_eq!(result, ExpiredTimelocks::None { blocks_left: 3 });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn transient_failures_are_retried_until_a_read_succeeds() {
        let calls = AtomicU32::new(0);
        let started = Instant::now();
        let result = expired_timelock_retrying(
            || async {
                if calls.fetch_add(1, Ordering::SeqCst) < 5 {
                    anyhow::bail!("Electrum: all servers failed")
                }
                Ok(ExpiredTimelocks::Cancel { blocks_left: 10 })
            },
            far(),
        )
        .await
        .unwrap();
        assert_eq!(result, ExpiredTimelocks::Cancel { blocks_left: 10 });
        assert_eq!(calls.load(Ordering::SeqCst), 6);
        // 15 + 30 + 45 + 60 + 60 seconds of backoff
        assert_eq!(started.elapsed(), Duration::from_secs(210));
    }

    #[tokio::test(start_paused = true)]
    async fn an_outage_of_hours_does_not_end_the_wait() {
        let calls = AtomicU32::new(0);
        let result = expired_timelock_retrying(
            || async {
                // fails for about 5 hours of reads
                if calls.fetch_add(1, Ordering::SeqCst) < 300 {
                    anyhow::bail!("connection refused")
                }
                Ok(ExpiredTimelocks::Punish)
            },
            far(),
        )
        .await;
        assert_eq!(result.unwrap(), ExpiredTimelocks::Punish);
    }

    #[tokio::test(start_paused = true)]
    async fn the_backoff_never_exceeds_a_minute() {
        let calls = AtomicU32::new(0);
        let started = Instant::now();
        expired_timelock_retrying(
            || async {
                if calls.fetch_add(1, Ordering::SeqCst) < 20 {
                    anyhow::bail!("timeout")
                }
                Ok(ExpiredTimelocks::None { blocks_left: 1 })
            },
            far(),
        )
        .await
        .unwrap();
        // 15 + 30 + 45, then 17 waits of 60 s
        assert_eq!(started.elapsed(), Duration::from_secs(90 + 17 * 60));
    }

    #[tokio::test(start_paused = true)]
    async fn it_gives_up_only_after_the_deadline() {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(600);
        let error = expired_timelock_retrying(
            || async { anyhow::bail!("Electrum: all servers failed") },
            deadline,
        )
        .await
        .unwrap_err();
        assert!(started.elapsed() >= Duration::from_secs(600));
        assert!(format!("{error:#}").contains("timed out reading the Bitcoin refund timelock"));
        assert!(format!("{error:#}").contains("all servers failed"));
    }
}

#[cfg(test)]
mod early_refund_tests {
    //! Audit 2026-11-10, H-3: an early refund that is seen but never confirms must not strand
    //! the buyer; the cancel timelock hands the swap back to cancel/refund.
    use super::{race_early_refund, wait_until_cancel_timelock_expired};
    use crate::protocol_v5::core::bitcoin::ExpiredTimelocks;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;
    use tokio::time::{sleep, Instant};

    async fn after(seconds: u64) -> anyhow::Result<()> {
        sleep(Duration::from_secs(seconds)).await;
        Ok(())
    }

    async fn never() -> anyhow::Result<()> {
        std::future::pending().await
    }

    #[tokio::test(start_paused = true)]
    async fn a_confirmation_before_the_timelock_is_a_refund() {
        assert!(race_early_refund(after(600), after(3_600)).await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn an_early_refund_that_never_confirms_gives_way_at_the_timelock() {
        let started = Instant::now();
        assert!(!race_early_refund(never(), after(7 * 3_600)).await.unwrap());
        assert_eq!(started.elapsed(), Duration::from_secs(7 * 3_600));
    }

    #[tokio::test(start_paused = true)]
    async fn a_confirmation_ready_at_the_same_moment_wins() {
        assert!(race_early_refund(after(0), after(0)).await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_subscription_is_an_error_not_a_refund() {
        let failed = async { anyhow::bail!("subscription closed") };
        assert!(race_early_refund(failed, never()).await.is_err());
    }

    fn reads(sequence: Vec<anyhow::Result<ExpiredTimelocks>>) -> impl FnMut() -> std::future::Ready<anyhow::Result<ExpiredTimelocks>> {
        let mut sequence = sequence.into_iter();
        move || std::future::ready(sequence.next().unwrap_or(Ok(ExpiredTimelocks::Punish)))
    }

    #[tokio::test(start_paused = true)]
    async fn the_timelock_wait_ends_when_the_cancel_timelock_expires() {
        let started = Instant::now();
        let read = reads(vec![
            Ok(ExpiredTimelocks::None { blocks_left: 2 }),
            Ok(ExpiredTimelocks::None { blocks_left: 1 }),
            Ok(ExpiredTimelocks::Cancel { blocks_left: 20 }),
        ]);
        wait_until_cancel_timelock_expired(read, Duration::from_secs(30)).await.unwrap();
        assert_eq!(started.elapsed(), Duration::from_secs(60));
    }

    #[tokio::test(start_paused = true)]
    async fn the_timelock_wait_survives_read_failures() {
        let read = reads(vec![
            Ok(ExpiredTimelocks::None { blocks_left: 1 }),
            Err(anyhow::anyhow!("Electrum: all servers failed")),
            Err(anyhow::anyhow!("timeout")),
            Ok(ExpiredTimelocks::Cancel { blocks_left: 5 }),
        ]);
        wait_until_cancel_timelock_expired(read, Duration::from_secs(30)).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn every_expired_state_counts_as_expired() {
        for state in [
            ExpiredTimelocks::Cancel { blocks_left: 1 },
            ExpiredTimelocks::WaitingForRemainingRefund { blocks_left: 1 },
            ExpiredTimelocks::RemainingRefund,
            ExpiredTimelocks::Punish,
        ] {
            let calls = AtomicU32::new(0);
            let read = || {
                calls.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Ok(state))
            };
            wait_until_cancel_timelock_expired(read, Duration::from_secs(30)).await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{state:?}");
        }
    }

    /// The transfer-proof wait must act on a confirmed early refund, not a seen one.
    #[test]
    fn the_transfer_proof_wait_acts_only_on_a_confirmed_early_refund() {
        let source = include_str!("bob.rs");
        let body = &source[source.find("let early_refund_status = bitcoin_wallet").unwrap()..];
        let body = &body[..body.find("anyhow::ensure!(").unwrap()];
        assert!(body.contains("early_refund_status.wait_until_confirmed_with(1u32)"));
        assert!(!body.contains("wait_until_seen"));
    }
}

#[cfg(test)]
mod witness_confirmation_tests {
    use super::witnesses_confirm_lock;
    use crate::monero::witness::{LockWitness, Witnesses};
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::time::Duration;

    /// A witness node that always reports `confirmations`.
    struct Node(&'static str, Option<u64>);

    #[async_trait]
    impl LockWitness for Node {
        fn name(&self) -> String {
            self.0.to_owned()
        }
        async fn lock_is_valid(&self, _: &str, _: &[u8; 32], _: &[u8], _: u64) -> Result<bool> {
            Ok(true)
        }
        async fn confirmations(&self, _: &str) -> Result<Option<u64>> {
            Ok(self.1)
        }
    }

    fn witnesses(nodes: Vec<(&'static str, Option<u64>)>) -> Option<Witnesses> {
        Some(Witnesses::new(
            nodes
                .into_iter()
                .map(|(name, confirmations)| Arc::new(Node(name, confirmations)) as Arc<dyn LockWitness>)
                .collect(),
        ))
    }

    async fn check(witnesses: Option<Witnesses>, required: bool) -> Result<()> {
        witnesses_confirm_lock(
            witnesses,
            required,
            "tx",
            10,
            Duration::from_millis(10),
            Duration::from_millis(60),
        )
        .await
    }

    /// The user's own node (Node Type): no witnesses, none required, nothing to wait for.
    #[tokio::test]
    async fn the_users_own_node_needs_no_witnesses() {
        check(None, false).await.unwrap();
        check(Some(Witnesses::new(vec![])), false).await.unwrap();
    }

    #[tokio::test]
    async fn required_witnesses_that_are_missing_stop_the_signature() {
        let error = check(None, true).await.unwrap_err();
        assert!(error.to_string().contains("required"), "{error:#}");
    }

    #[tokio::test]
    async fn witnesses_that_see_the_lock_confirmed_let_the_signature_go() {
        check(witnesses(vec![("a", Some(10)), ("b", Some(12))]), true).await.unwrap();
    }

    #[tokio::test]
    async fn a_witness_still_behind_at_the_deadline_stops_the_signature() {
        let error = check(witnesses(vec![("a", Some(10)), ("b", Some(3))]), true)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("b (3/10)"), "{error:#}");
        assert!(check(witnesses(vec![("a", None)]), false).await.is_err());
    }

    /// Source check: the main path runs the witness check before the signature. The resume
    /// arms are checked in the app crate (`resume_v5::witness_check_tests`).
    #[test]
    fn the_main_path_checks_the_witnesses_before_the_signature() {
        let main = include_str!("bob.rs");
        let main = &main[main.find("pub async fn run_until_signature_sent").unwrap()..];
        let main = &main[..main.find("send_redeem_encrypted_signature(").unwrap()];
        assert!(main.contains("witnesses_confirm_lock_for("));
    }
}

#[cfg(test)]
mod deposit_step_tests {
    use super::{deposit_step, DepositStatuses, DepositStep};
    use bitcoin_wallet::primitives::Confirmed;
    use bitcoin_wallet::ScriptStatus::{self, InMempool, Unseen};

    /// Remaining-refund timelock used here, as on mainnet and testnet.
    const TIMELOCK: u32 = 2;

    fn confirmations(n: u32) -> ScriptStatus {
        ScriptStatus::Confirmed(Confirmed::new(n - 1))
    }

    fn statuses(
        partial_refund: ScriptStatus,
        reclaim: ScriptStatus,
        withhold: ScriptStatus,
        mercy: ScriptStatus,
    ) -> DepositStatuses {
        DepositStatuses { partial_refund, reclaim, withhold, mercy }
    }

    #[test]
    fn the_deposit_waits_for_its_timelock() {
        assert_eq!(
            deposit_step(&statuses(confirmations(1), Unseen, Unseen, Unseen), TIMELOCK),
            DepositStep::WaitForTimelock { blocks_left: 1 }
        );
        assert_eq!(
            deposit_step(&statuses(InMempool, Unseen, Unseen, Unseen), TIMELOCK),
            DepositStep::WaitForTimelock { blocks_left: 2 }
        );
    }

    #[test]
    fn once_the_timelock_has_passed_the_reclaim_is_published() {
        assert_eq!(
            deposit_step(&statuses(confirmations(2), Unseen, Unseen, Unseen), TIMELOCK),
            DepositStep::PublishReclaim
        );
    }

    #[test]
    fn a_reclaim_in_the_mempool_is_waited_for_and_a_confirmed_one_ends_it() {
        assert_eq!(
            deposit_step(&statuses(confirmations(3), InMempool, Unseen, Unseen), TIMELOCK),
            DepositStep::WaitForReclaim
        );
        assert_eq!(
            deposit_step(&statuses(confirmations(3), confirmations(1), Unseen, Unseen), TIMELOCK),
            DepositStep::Reclaimed
        );
    }

    #[test]
    fn a_withhold_in_the_mempool_stops_the_reclaim() {
        // even with the timelock passed: a reclaim would only conflict
        assert_eq!(
            deposit_step(&statuses(confirmations(5), Unseen, InMempool, Unseen), TIMELOCK),
            DepositStep::WaitForWithhold
        );
    }

    #[test]
    fn a_confirmed_withhold_ends_it_unless_mercy_follows() {
        assert_eq!(
            deposit_step(&statuses(confirmations(5), Unseen, confirmations(1), Unseen), TIMELOCK),
            DepositStep::Withheld
        );
        assert_eq!(
            deposit_step(&statuses(confirmations(5), Unseen, confirmations(1), InMempool), TIMELOCK),
            DepositStep::WaitForMercy
        );
        assert_eq!(
            deposit_step(
                &statuses(confirmations(5), Unseen, confirmations(2), confirmations(1)),
                TIMELOCK
            ),
            DepositStep::MercyConfirmed
        );
    }

    /// Upstream would keep waiting for a reclaim that lost to a confirmed withhold.
    #[test]
    fn a_confirmed_withhold_wins_over_a_reclaim_still_in_the_mempool() {
        assert_eq!(
            deposit_step(
                &statuses(confirmations(5), InMempool, confirmations(1), Unseen),
                TIMELOCK
            ),
            DepositStep::Withheld
        );
    }
}

#[cfg(test)]
mod early_refund_check_tests {
    use super::{early_refund_check, EarlyRefundCheck};
    use bitcoin_wallet::primitives::Confirmed;
    use bitcoin_wallet::ScriptStatus;

    #[test]
    fn a_confirmed_early_refund_ends_the_swap() {
        assert_eq!(
            early_refund_check(&ScriptStatus::Confirmed(Confirmed::new(0))),
            EarlyRefundCheck::Refunded
        );
    }

    #[test]
    fn an_early_refund_in_the_mempool_is_waited_for() {
        assert_eq!(early_refund_check(&ScriptStatus::InMempool), EarlyRefundCheck::Pending);
    }

    /// The handover records the timelock, not a confirmed cancel that does not exist
    /// (source check: the wait needs a live subscription, which no test wallet has).
    #[test]
    fn the_handover_records_the_cancel_timelock_not_a_cancel() {
        let source = include_str!("bob.rs");
        let start = source.find("pub async fn wait_for_early_refund_confirmation(").unwrap();
        let body = &source[start..start + source[start..].find("\n}\n").unwrap()];
        assert!(body.contains("db::bob::Bob::CancelTimelockExpired(state6.clone())"));
        assert!(!body.contains("Bob::BtcCancelled"));
    }

    #[test]
    fn no_early_refund_means_cancel_and_refund_as_before() {
        assert_eq!(early_refund_check(&ScriptStatus::Unseen), EarlyRefundCheck::Absent);
        assert_eq!(early_refund_check(&ScriptStatus::Retrying), EarlyRefundCheck::Absent);
    }
}

#[cfg(test)]
mod punish_window_tests {
    //! Audit 2026-11-10, H-5.
    use super::{punish_window_action, PunishWindowAction, Punished};
    use bitcoin_wallet::primitives::Confirmed;
    use bitcoin_wallet::ScriptStatus;

    #[test]
    fn only_a_confirmed_punish_ends_the_swap() {
        assert_eq!(
            punish_window_action(&Ok(ScriptStatus::Confirmed(Confirmed::new(0)))),
            PunishWindowAction::RecordPunished
        );
        assert_eq!(
            punish_window_action(&Ok(ScriptStatus::Confirmed(Confirmed::new(5)))),
            PunishWindowAction::RecordPunished
        );
    }

    #[test]
    fn an_unseen_punish_leaves_the_refund_to_try() {
        assert_eq!(punish_window_action(&Ok(ScriptStatus::Unseen)), PunishWindowAction::TryRefund);
    }

    #[test]
    fn a_punish_in_the_mempool_does_not_end_the_swap_yet() {
        // the refund may still win; the next round checks again
        assert_eq!(punish_window_action(&Ok(ScriptStatus::InMempool)), PunishWindowAction::TryRefund);
        assert_eq!(punish_window_action(&Ok(ScriptStatus::Retrying)), PunishWindowAction::TryRefund);
    }

    #[test]
    fn a_failed_status_read_is_not_a_punish() {
        assert_eq!(
            punish_window_action(&Err(anyhow::anyhow!("Electrum: all servers failed"))),
            PunishWindowAction::TryRefund
        );
    }

    #[test]
    fn a_punish_is_a_typed_error_callers_can_recognise() {
        let txid: bitcoin::Txid =
            "0000000000000000000000000000000000000000000000000000000000000001".parse().unwrap();
        let error: anyhow::Error = Punished(txid).into();
        let error = error.context("Failed to cancel/refund Bitcoin");
        assert!(error.downcast_ref::<Punished>().is_some());
        assert!(format!("{error:#}").contains("punished"));
    }

    /// The refund loop must ask before recording a punish.
    #[test]
    fn the_refund_loop_checks_the_punish_transaction_first() {
        let source = include_str!("bob.rs");
        let body = &source[source.find("pub async fn run_cancel_and_refund(").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let arm = &body[body.find("ExpiredTimelocks::Punish =>").unwrap()..];
        let check = arm.find("punish_window_action(").unwrap();
        let record = arm.find("persist_punished(").unwrap();
        assert!(check < record);
        assert!(arm.contains("publish_and_confirm_refund("));
    }
}

#[cfg(test)]
mod xmr_received_tests {
    //! Matrix N-15: the amount on the Swap Success screen. The fixture is a real stagenet
    //! sweep (block 2246652, swap `01a611e8`); its fee, 121,760,000 piconero, was decoded from
    //! the wire bytes by a separate script, not by the code under test.
    use super::*;

    const SWEEP_HEX: &str =
        include_str!("../../../protocol-v5/swap-serde/tests/fixtures/stagenet_sweep_tx.hex");
    const SWEEP_FEE: u64 = 121_760_000;

    fn stored_sweep() -> crate::monero::ng::ConstructedSweep {
        crate::monero::ng::ConstructedSweep {
            tx_hash: crate::monero::TxHash(
                "ed134b4707d78f224145228178539c0c76463290defef9e7ff5af1189d935644".into(),
            ),
            tx_hex: SWEEP_HEX.trim().into(),
        }
    }

    #[test]
    fn the_fee_is_read_from_the_signed_sweep() {
        assert_eq!(stored_sweep().fee_piconero().unwrap(), SWEEP_FEE);
    }

    #[test]
    fn the_buyer_receives_the_locked_amount_minus_the_fee() {
        let locked = 125_300_000_000; // 0.1253 XMR
        let got = received_after_fee(locked, stored_sweep().fee_piconero().unwrap()).unwrap();
        assert_eq!(got.fee_piconero, SWEEP_FEE);
        assert_eq!(got.received_piconero, locked - SWEEP_FEE);
        assert_eq!(got.received_piconero + got.fee_piconero, locked);
    }

    #[test]
    fn a_fee_above_the_locked_amount_gives_no_number() {
        assert!(received_after_fee(SWEEP_FEE - 1, SWEEP_FEE).is_err());
        assert_eq!(
            received_after_fee(SWEEP_FEE, SWEEP_FEE).unwrap().received_piconero,
            0
        );
    }

    #[test]
    fn a_damaged_sweep_gives_no_fee() {
        let mut sweep = stored_sweep();
        sweep.tx_hex.truncate(sweep.tx_hex.len() - 2);
        assert!(sweep.fee_piconero().is_err());
    }
}
