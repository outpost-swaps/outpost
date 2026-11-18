use crate::asb::asb_xmr_balance_data::AsbXmrBalanceData;
use crate::asb::liquidity::Reservations;
use crate::asb::{Behaviour, OutEvent, Rate};
use crate::monero::Amount;
use crate::network::quote::{BidQuote, RefundPolicyWire};
use crate::network::rendezvous;
use crate::network::swap_setup::alice::WalletSnapshot;
use crate::network::transfer_proof;
use crate::protocol::alice::swap::has_already_processed_enc_sig;
use crate::protocol::alice::{AliceState, State3, Swap};
use crate::protocol::{Database, State};
use crate::{bitcoin, env, kraken, monero, util};
use anyhow::{anyhow, Context, Result};
use futures::future;
use futures::future::{BoxFuture, FutureExt};
use futures::stream::{FuturesUnordered, StreamExt};
use libp2p::request_response::{OutboundFailure, OutboundRequestId, ResponseChannel};
use libp2p::swarm::SwarmEvent;
use libp2p::{PeerId, Swarm};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::convert::{Infallible, TryInto};
use std::fmt::Debug;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

const ASB_WALLET_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const QUOTE_REQUEST_MIN_INTERVAL: Duration = Duration::from_secs(1);

fn fresh_cached_quote<'a>(
    cached: Option<&'a (Instant, BidQuote)>,
    now: Instant,
) -> Option<&'a BidQuote> {
    cached
        .filter(|(cached_at, _)| now.duration_since(*cached_at) < QUOTE_REQUEST_MIN_INTERVAL)
        .map(|(_, quote)| quote)
}

#[allow(missing_debug_implementations)]
pub struct EventLoop<LR>
where
    LR: LatestRate + Send + 'static + Debug + Clone,
{
    swarm: libp2p::Swarm<Behaviour<LR>>,
    env_config: env::Config,
    bitcoin_wallet: Arc<bitcoin::Wallet>,
    monero_wallet: Arc<monero::Wallet>,
    db: Arc<dyn Database + Send + Sync>,
    latest_rate: LR,
    min_buy: bitcoin::Amount,
    max_buy: bitcoin::Amount,
    external_redeem_address: Option<bitcoin::Address>,

    swap_sender: mpsc::Sender<Swap>,

    /// Shared Monero liquidity reservations: XMR promised to in-flight swaps
    /// (reserved at setup acceptance, re-keyed to the swap id, released when
    /// the swap task exits) so concurrent setups cannot over-promise the same
    /// unlocked balance.
    liquidity: Arc<Reservations>,

    /// Stores where to send [`EncryptedSignature`]s to
    /// The corresponding receiver for this channel is stored in the EventLoopHandle
    /// that is responsible for the swap.
    ///
    /// Once a [`EncryptedSignature`] has been sent to the EventLoopHandle,
    /// the sender is removed from this map.
    recv_encrypted_signature: HashMap<Uuid, bmrng::RequestSender<bitcoin::EncryptedSignature, ()>>,

    /// Once we receive an [`EncryptedSignature`] from Bob, we forward it to the EventLoopHandle.
    /// Once the EventLoopHandle acknowledges the receipt of the [`EncryptedSignature`], we need to confirm this to Bob.
    /// When the EventLoopHandle acknowledges the receipt, a future in this collection resolves and returns the libp2p channel
    /// which we use to confirm to Bob that we have received the [`EncryptedSignature`].
    ///
    /// Flow:
    /// 1. When signature forwarded via recv_encrypted_signature sender
    /// 2. New future pushed here to await EventLoopHandle's acknowledgement
    /// 3. When future completes, the EventLoop uses the ResponseChannel to send an acknowledgment to Bob
    /// 4. Future is removed from this collection
    inflight_encrypted_signatures: FuturesUnordered<BoxFuture<'static, ResponseChannel<()>>>,

    /// Channel for sending transfer proofs to Bobs. The sender is shared with every EventLoopHandle.
    /// The receiver is polled by the event loop to send transfer proofs over the network to Bob.
    ///
    /// Flow:
    /// 1. EventLoopHandle sends (PeerId, Request, Responder) through sender
    /// 2. Event loop receives and attempts to send to peer
    /// 3. Result (Ok or network failure) is sent back to EventLoopHandle
    #[allow(clippy::type_complexity)]
    outgoing_transfer_proofs_requests: tokio::sync::mpsc::UnboundedReceiver<(
        PeerId,
        transfer_proof::Request,
        oneshot::Sender<Result<(), OutboundFailure>>,
    )>,
    #[allow(clippy::type_complexity)]
    outgoing_transfer_proofs_sender: tokio::sync::mpsc::UnboundedSender<(
        PeerId,
        transfer_proof::Request,
        oneshot::Sender<Result<(), OutboundFailure>>,
    )>,

    /// Temporarily stores transfer proof requests for peers that are currently disconnected.
    ///
    /// When a transfer proof cannot be sent because there's no connection to the peer:
    /// 1. It is moved from [`outgoing_transfer_proofs_requests`] to this buffer
    /// 2. Once a connection is established with the peer, the proof is send back into the [`outgoing_transfer_proofs_sender`]
    /// 3. The buffered request is then removed from this collection
    #[allow(clippy::type_complexity)]
    buffered_transfer_proofs: HashMap<
        PeerId,
        Vec<(
            transfer_proof::Request,
            oneshot::Sender<Result<(), OutboundFailure>>,
        )>,
    >,

    /// Tracks [`transfer_proof::Request`]s which are currently inflight and awaiting an acknowledgement from Bob
    ///
    /// When a transfer proof is sent to Bob:
    /// 1. A unique request ID is generated by libp2p
    /// 2. The response channel is stored in this map with the request ID as key
    /// 3. When Bob acknowledges the proof, we use the stored channel to notify the EventLoopHandle
    /// 4. The entry is then removed from this map
    inflight_transfer_proofs:
        HashMap<OutboundRequestId, oneshot::Sender<Result<(), OutboundFailure>>>,

    /// Set once the "Monero balance too low" warning has been emitted for the
    /// current low-balance episode: quotes are recomputed on every seller-list
    /// poll (seconds apart), which otherwise spams the log with the same
    /// warning indefinitely. Cleared once the balance is sufficient again, so
    /// a later drop warns once more.
    low_balance_warned: bool,

    /// Coalesce concurrent quote requests: serve this quote for the short
    /// throttle window instead of dropping duplicate requests. Dropping them
    /// makes the GUI's parallel seller-list probes see the local ASB as
    /// unreachable.
    last_quote: Option<(Instant, BidQuote)>,
}

impl<LR> EventLoop<LR>
where
    LR: LatestRate + Send + 'static + Debug + Clone,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        swarm: Swarm<Behaviour<LR>>,
        env_config: env::Config,
        bitcoin_wallet: Arc<bitcoin::Wallet>,
        monero_wallet: Arc<monero::Wallet>,
        db: Arc<dyn Database + Send + Sync>,
        latest_rate: LR,
        min_buy: bitcoin::Amount,
        max_buy: bitcoin::Amount,
        external_redeem_address: Option<bitcoin::Address>,
        liquidity: Arc<Reservations>,
    ) -> Result<(Self, mpsc::Receiver<Swap>)> {
        let swap_channel = MpscChannels::default();
        let (outgoing_transfer_proofs_sender, outgoing_transfer_proofs_requests) =
            tokio::sync::mpsc::unbounded_channel();

        let event_loop = EventLoop {
            swarm,
            env_config,
            bitcoin_wallet,
            monero_wallet,
            db,
            latest_rate,
            swap_sender: swap_channel.sender,
            liquidity,
            min_buy,
            max_buy,
            external_redeem_address,
            recv_encrypted_signature: Default::default(),
            inflight_encrypted_signatures: Default::default(),
            outgoing_transfer_proofs_requests,
            outgoing_transfer_proofs_sender,
            buffered_transfer_proofs: Default::default(),
            inflight_transfer_proofs: Default::default(),
            low_balance_warned: false,
            last_quote: None,
        };
        Ok((event_loop, swap_channel.receiver))
    }

    pub fn peer_id(&self) -> PeerId {
        *Swarm::local_peer_id(&self.swarm)
    }

    pub async fn run(mut self) {
        // ensure that these streams are NEVER empty, otherwise it will
        // terminate forever.
        self.inflight_encrypted_signatures
            .push(future::pending().boxed());

        let swaps = match self.db.all().await {
            Ok(swaps) => swaps,
            Err(e) => {
                tracing::error!("Failed to load swaps from database: {}", e);
                return;
            }
        };

        let unfinished_swaps = swaps
            .into_iter()
            .filter(|(_swap_id, state)| !state.swap_finished())
            .collect::<Vec<(Uuid, State)>>();

        for (swap_id, state) in unfinished_swaps {
            let peer_id = match self.db.get_peer_id(swap_id).await {
                Ok(peer_id) => peer_id,
                Err(_) => {
                    tracing::warn!(%swap_id, "Resuming swap skipped because no peer-id found for swap in database");
                    continue;
                }
            };

            let alice_state: AliceState = state
                .clone()
                .try_into()
                .expect("Alice state loaded from db");

            // Re-reserve the XMR of swaps that have not yet locked it so
            // that resumed swaps cannot over-promise liquidity that newer
            // setups may otherwise consume. Best-effort: if the wallet
            // cannot be read we still resume the swap.
            if let Some(state3) = alice_state.pre_xmr_lock_state3() {
                let balance = tokio::time::timeout(
                    ASB_WALLET_RPC_TIMEOUT,
                    self.monero_wallet.get_balance(),
                )
                .await;
                match balance {
                    Ok(Ok(balance)) => {
                        if let Err(error) = self.liquidity.reserve(
                            swap_id,
                            state3.xmr_amount(),
                            Amount::from_piconero(balance.unlocked_balance),
                            monero::MONERO_FEE,
                        ) {
                            tracing::warn!(
                                %swap_id,
                                "Startup liquidity re-reservation failed (swap resumes anyway): {}",
                                error
                            );
                            // The BTC lock is already funded; force the
                            // accounting entry so new quotes cannot spend the
                            // same XMR even though this swap is over-committed.
                            self.liquidity.force_reserve(
                                swap_id,
                                state3.xmr_amount(),
                                monero::MONERO_FEE,
                            );
                        } else {
                            tracing::info!(
                                %swap_id,
                                amount = %state3.xmr_amount(),
                                "ASB_LIQUIDITY_RERESERVED for resumed pre-lock swap"
                            );
                        }
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(
                            %swap_id,
                            "Startup liquidity re-reservation failed, wallet balance unavailable (swap resumes anyway): {:#}",
                            error
                        );
                        self.liquidity.force_reserve(
                            swap_id,
                            state3.xmr_amount(),
                            monero::MONERO_FEE,
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            %swap_id,
                            "Startup liquidity re-reservation timed out (swap resumes anyway)"
                        );
                        self.liquidity.force_reserve(
                            swap_id,
                            state3.xmr_amount(),
                            monero::MONERO_FEE,
                        );
                    }
                }
            }

            let handle = self.new_handle(peer_id, swap_id);

            let swap = Swap {
                event_loop_handle: handle,
                bitcoin_wallet: self.bitcoin_wallet.clone(),
                monero_wallet: self.monero_wallet.clone(),
                env_config: self.env_config,
                db: self.db.clone(),
                state: alice_state,
                swap_id,
            };

            match self.swap_sender.send(swap).await {
                Ok(_) => tracing::info!(%swap_id, "Resuming swap"),
                Err(_) => {
                    tracing::warn!(%swap_id, "Failed to resume swap because receiver has been dropped")
                }
            }
        }

        // Resurrect safely-aborted swaps whose Bitcoin lock is funded: the
        // 2-hour lock-confirmation listener may have expired while the lock
        // tx was still unconfirmed, but the protocol allows Alice to lock her
        // XMR any time before the cancel timelock. Re-broadcast the lock tx
        // (it may not have propagated to miners) and resume the swap from the
        // last keyed state row; the state machine then triages the timelocks
        // itself (late XMR lock / cancel / punish).
        let all_swaps = match self.db.all().await {
            Ok(swaps) => swaps,
            Err(error) => {
                tracing::error!(
                    "Failed to load swaps from database for resurrection: {:#}",
                    error
                );
                Vec::new()
            }
        };
        for (swap_id, state) in all_swaps {
            let alice_state: AliceState = match state.try_into() {
                Ok(alice_state) => alice_state,
                Err(_) => continue,
            };
            if !matches!(alice_state, AliceState::SafelyAborted) {
                continue;
            }

            let peer_id = match self.db.get_peer_id(swap_id).await {
                Ok(peer_id) => peer_id,
                Err(_) => continue,
            };

            let state3 =
                match super::recovery::punish::recover_latest_state3(swap_id, &self.db).await {
                    Ok(state3) => state3,
                    Err(_) => continue,
                };

            // Only resurrect while Alice can still lock XMR. Once the cancel
            // timelock starts expiring, resuming cannot recover the swap: the
            // auto-punish sweep handles a spent lock. Without this guard an
            // abandoned swap whose lock output is already spent replays its
            // terminal transition on every ASB startup.
            match state3.expired_timelocks(&self.bitcoin_wallet).await {
                Ok(bitcoin::ExpiredTimelocks::None) => {}
                Ok(epoch) => {
                    tracing::info!(
                        %swap_id,
                        ?epoch,
                        "Skipping resurrection of safely-aborted swap: timelock has started expiring"
                    );
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        %swap_id,
                        %error,
                        "Skipping resurrection of safely-aborted swap: could not check timelocks"
                    );
                    continue;
                }
            }

            // Best-effort re-broadcast of the (possibly unconfirmed) lock tx;
            // "already in block chain" errors mean it is confirmed and are fine.
            let lock_psbt: bitcoin::PartiallySignedTransaction = state3.tx_lock.clone().into();
            match self
                .bitcoin_wallet
                .broadcast(lock_psbt.extract_tx().expect("lock PSBT is fully signed"), "lock-resurrect")
                .await
            {
                Ok(_) => {
                    tracing::info!(%swap_id, "Re-broadcast Bitcoin lock transaction while resurrecting safely-aborted swap")
                }
                Err(error) => tracing::debug!(%swap_id, "Lock re-broadcast skipped: {:#}", error),
            }

            let handle = self.new_handle(peer_id, swap_id);

            // The swap is resurrected before its XMR lock - reserve the
            // liquidity it will spend. Forced: the BTC lock is already
            // funded, so the swap will proceed regardless of what else is
            // reserved (accounting only).
            self.liquidity
                .force_reserve(swap_id, state3.xmr_amount(), monero::MONERO_FEE);

            let swap = Swap {
                event_loop_handle: handle,
                bitcoin_wallet: self.bitcoin_wallet.clone(),
                monero_wallet: self.monero_wallet.clone(),
                env_config: self.env_config,
                db: self.db.clone(),
                state: AliceState::BtcLocked { state3 },
                swap_id,
            };

            match self.swap_sender.send(swap).await {
                Ok(_) => {
                    tracing::info!(%swap_id, "Resurrecting safely-aborted swap with funded Bitcoin lock")
                }
                Err(_) => {
                    tracing::warn!(%swap_id, "Failed to resurrect swap because receiver has been dropped")
                }
            }
        }

        let mut last_time_checked_in_secs = 0;
        loop {
            let current_time_in_secs = util::get_sys_time_in_secs();
            let time_since_last_check = current_time_in_secs.saturating_sub(last_time_checked_in_secs);
            if time_since_last_check >= 5 {
                match tokio::time::timeout(
                    ASB_WALLET_RPC_TIMEOUT,
                    self.monero_wallet.get_balance(),
                )
                .await
                {
                    Ok(Ok(balance)) => {
                        tracing::info!(%balance.balance, %balance.unlocked_balance, "ASB_XMR_BALANCE_DATA");
                    }
                    Ok(Err(err)) => {
                        let error = err.to_string();
                        tracing::info!(%error, "ASB_XMR_BALANCE_DATA" );
                    }
                    Err(_) => {
                        tracing::warn!("ASB_XMR_BALANCE_DATA timed out waiting for the wallet RPC");
                    }
                };
                last_time_checked_in_secs = util::get_sys_time_in_secs();
            }

            tokio::select! {
                biased;
                swarm_event = self.swarm.select_next_some() => {
                    match swarm_event {
                        SwarmEvent::Behaviour(OutEvent::SwapSetupInitiated { mut send_wallet_snapshot }) => {

                            let (btc, responder) = match send_wallet_snapshot.recv().await {
                                Ok((btc, responder)) => (btc, responder),
                                Err(error) => {
                                    tracing::error!("Swap request will be ignored because of a failure when requesting information for the wallet snapshot: {:#}", error);
                                    continue;
                                }
                            };

                            let wallet_snapshot = match WalletSnapshot::capture(&self.bitcoin_wallet, &self.monero_wallet, &self.external_redeem_address, btc).await {
                                Ok(wallet_snapshot) => wallet_snapshot,
                                Err(error) => {
                                    tracing::error!("Swap request will be ignored because we were unable to create wallet snapshot for swap: {:#}", error);
                                    continue;
                                }
                            };

                            // Ignore result, we should never hit this because the receiver will alive as long as the connection is.
                            let _ = responder.respond(wallet_snapshot);
                        }
                        SwarmEvent::Behaviour(OutEvent::SwapSetupCompleted{peer_id, swap_id, state3, liquidity_key}) => {
                            // Save the peer id before re-keying anything: the
                            // swap_id primary key also rejects replayed/duplicate
                            // setup ids. If this fails, release the original
                            // setup-acceptance reservation and do not start a
                            // task that cannot be resumed.
                            if let Err(error) = self.db.insert_peer_id(swap_id, peer_id).await {
                                if let Some(key) = liquidity_key {
                                    if let Some(amount) = self.liquidity.release(key) {
                                        tracing::warn!(%swap_id, %amount, "ASB_LIQUIDITY_RELEASED_DUPLICATE_OR_DB_FAILURE");
                                    }
                                }
                                tracing::warn!(
                                    %swap_id,
                                    "Rejecting swap setup: unable to save peer-id (duplicate swap id or database error): {}",
                                    error
                                );
                                continue;
                            }

                            // Re-key the setup-acceptance reservation to the
                            // swap id so the release on task exit finds it.
                            match liquidity_key.map(|key| self.liquidity.transfer(key, swap_id)) {
                                Some(Some(amount)) => {
                                    tracing::info!(%swap_id, %amount, "ASB_LIQUIDITY_KEYED to the swap id");
                                }
                                Some(None) => {
                                    tracing::warn!(%swap_id, "Setup-completed liquidity reservation not found; re-reserving from current wallet balance");
                                    // Fall back to a forced reservation so
                                    // accounting stays correct.
                                    self.liquidity
                .force_reserve(swap_id, state3.xmr_amount(), monero::MONERO_FEE);
                                }
                                None => {
                                    tracing::warn!(%swap_id, "Setup completed without a liquidity reservation; re-reserving from current wallet balance");
                                    self.liquidity
                .force_reserve(swap_id, state3.xmr_amount(), monero::MONERO_FEE);
                                }
                            }
                            self.handle_execution_setup_done(peer_id, swap_id, state3).await;
                        }
                        SwarmEvent::Behaviour(OutEvent::SwapDeclined { peer, error }) => {
                            tracing::warn!(%peer, "Ignoring spot price request: {}", error);
                        }
                        SwarmEvent::Behaviour(OutEvent::QuoteRequested { channel, peer }) => {
                            let now = Instant::now();
                            if let Some(quote) = fresh_cached_quote(self.last_quote.as_ref(), now) {
                                tracing::debug!(%peer, "Reusing recent quote");
                                if self
                                    .swarm
                                    .behaviour_mut()
                                    .quote
                                    .send_response(channel, quote.clone())
                                    .is_err()
                                {
                                    tracing::debug!(%peer, "Failed to respond with cached quote");
                                }
                                continue;
                            }

                            let quote = match self.make_quote(self.min_buy, self.max_buy).await {
                                Ok(quote) => quote,
                                Err(error) => {
                                    tracing::warn!(%peer, "Failed to make quote: {:#}", error);
                                    continue;
                                }
                            };
                            self.last_quote = Some((now, quote.clone()));

                            if self.swarm.behaviour_mut().quote.send_response(channel, quote).is_err() {
                                tracing::debug!(%peer, "Failed to respond with quote");
                            }
                        }
                        SwarmEvent::Behaviour(OutEvent::TransferProofAcknowledged { peer, id }) => {
                            tracing::info!(%peer, swap_id = %id, "ASB_TRANSFER_PROOF_ACK: Bob acknowledged the Monero lock proof");

                            if let Some(responder) = self.inflight_transfer_proofs.remove(&id) {
                                let _ = responder.send(Ok(()));
                            }
                        }
                        SwarmEvent::Behaviour(OutEvent::EncryptedSignatureReceived{ msg, channel, peer }) => {
                            let swap_id = msg.swap_id;
                            let swap_peer = self.db.get_peer_id(swap_id).await;

                            // Ensure that an incoming encrypted signature is sent by the peer-id associated with the swap
                            let swap_peer = match swap_peer {
                                Ok(swap_peer) => swap_peer,
                                Err(_) => {
                                    tracing::warn!(
                                        unknown_swap_id = %swap_id,
                                        from = %peer,
                                        "Ignoring encrypted signature for unknown swap");
                                    continue;
                                }
                            };

                            if swap_peer != peer {
                                tracing::warn!(
                                    %swap_id,
                                    received_from = %peer,
                                    expected_from = %swap_peer,
                                    "Ignoring malicious encrypted signature which was not expected from this peer",
                                    );
                                continue;
                            }

                            // Immediately acknowledge if we've already processed this encrypted signature
                            // This handles the case where Bob didn't receive our previous acknowledgment
                            // and is retrying sending the encrypted signature
                            if let Ok(state) = self.db.get_state(swap_id).await {
                                let state: AliceState = state.try_into()
                                    .expect("Alices database only contains Alice states");

                                // Check if we have already processed the encrypted signature
                                if has_already_processed_enc_sig(&state) {
                                    tracing::warn!(%swap_id, "Received encrypted signature for swap in state {}. We have already processed this encrypted signature. Acknowledging immediately.", state);

                                    // We push create a future that will resolve immediately, and returns the channel
                                    // This will be resolved in the next iteration of the event loop, and the acknowledgment will be sent to Bob
                                    self.inflight_encrypted_signatures.push(async move {
                                        channel
                                    }.boxed());

                                    continue;
                                }
                            }

                            let sender = match self.recv_encrypted_signature.remove(&swap_id) {
                                Some(sender) => sender,
                                None => {
                                    // TODO: Don't just drop encsig if we currently don't have a running swap for it, save in db
                                    // 1. Save the encrypted signature in the database
                                    // 2. Acknowledge the receipt of the encrypted signature
                                    tracing::warn!(%swap_id, "No sender for encrypted signature, maybe already handled?");
                                    continue;
                                }
                            };

                            let mut responder = match sender.send(msg.tx_redeem_encsig).await {
                                Ok(responder) => responder,
                                Err(_) => {
                                    tracing::warn!(%swap_id, "Failed to relay encrypted signature to swap");
                                    continue;
                                }
                            };

                            self.inflight_encrypted_signatures.push(async move {
                                let _ = responder.recv().await;

                                channel
                            }.boxed());
                        }
                        SwarmEvent::Behaviour(OutEvent::Rendezvous(rendezvous::register::Event::Registered { peer_id })) => {
                            tracing::trace!("Successfully registered with rendezvous node: {}", peer_id);
                        }
                        SwarmEvent::Behaviour(OutEvent::Rendezvous(rendezvous::register::Event::RegisterRequestFailed { peer_id, error })) => {
                            tracing::error!("Registration request with rendezvous node {} failed: {:?}", peer_id, error);
                        }
                        SwarmEvent::Behaviour(OutEvent::Rendezvous(rendezvous::register::Event::RegisterDispatchFailed { peer_id, error })) => {
                            tracing::warn!("Failed to dispatch registration with rendezvous node {}: {:?}", peer_id, error);
                        }
                        SwarmEvent::Behaviour(OutEvent::OutboundRequestResponseFailure {peer, error, request_id, protocol}) => {
                            tracing::error!(
                                %peer,
                                %request_id,
                                %error,
                                %protocol,
                                "Failed to send request-response request to peer");

                            if let Some(responder) = self.inflight_transfer_proofs.remove(&request_id) {
                                let _ = responder.send(Err(error));
                            }
                        }
                        SwarmEvent::Behaviour(OutEvent::InboundRequestResponseFailure {peer, error, request_id, protocol}) => {
                            tracing::error!(
                                %peer,
                                %request_id,
                                %error,
                                %protocol,
                                "Failed to receive request-response request from peer");
                        }
                        SwarmEvent::Behaviour(OutEvent::Failure {peer, error}) => {
                            tracing::error!(
                                %peer,
                                "Communication error: {:#}", error);
                        }
                        SwarmEvent::ConnectionEstablished { peer_id: peer, endpoint, .. } => {
                            tracing::debug!(%peer, address = %endpoint.get_remote_address(), "New connection established");

                            // If we have buffered transfer proofs for this peer, we can now send them
                            if let Some(transfer_proofs) = self.buffered_transfer_proofs.remove(&peer) {
                                for (transfer_proof, responder) in transfer_proofs {
                                    tracing::debug!(%peer, "Found buffered transfer proof for peer");

                                    // We have an established connection to the peer, so we can add the transfer proof to the queue
                                    // This is then polled in the next iteration of the event loop, and attempted to be sent to the peer
                                    if let Err(e) = self.outgoing_transfer_proofs_sender.send((peer, transfer_proof, responder)) {
                                        tracing::error!(%peer, error = %e, "Failed to forward buffered transfer proof to event loop channel");
                                    }
                                }
                            }
                        }
                        SwarmEvent::IncomingConnectionError { send_back_addr: address, error, .. } => {
                            if let libp2p::swarm::ListenError::Denied { cause } = &error {
                                if let Some(exceeded) = cause.downcast_ref::<libp2p::connection_limits::Exceeded>() {
                                    // DoS hardening: this is the connection-limits behaviour doing its job.
                                    // See notes/libp2p-upgrade-review.md §11.
                                    tracing::warn!(%address, error = %exceeded, "Rejected inbound connection to prevent against denial-of-service");
                                } else {
                                    tracing::trace!(%address, "Failed to set up connection with peer: {:?}", error);
                                }
                            } else {
                                tracing::trace!(%address, "Failed to set up connection with peer: {:?}", error);
                            }
                        }
                        SwarmEvent::ConnectionClosed { peer_id: peer, num_established: 0, endpoint, cause: Some(error), connection_id } => {
                            tracing::debug!(%peer, address = %endpoint.get_remote_address(), %connection_id, "Lost connection to peer: {:#}", error);
                        }
                        SwarmEvent::ConnectionClosed { peer_id: peer, num_established: 0, endpoint, cause: None, connection_id } => {
                            tracing::info!(%peer, address = %endpoint.get_remote_address(), %connection_id,  "Successfully closed connection");
                        }
                        SwarmEvent::NewListenAddr{address, ..} => {
                            tracing::info!(%address, "New listen address reported");
                        }
                        _ => {}
                    }
                },
                Some((peer, transfer_proof, responder)) = self.outgoing_transfer_proofs_requests.recv() => {
                    // If we are not connected to the peer, we buffer the transfer proof
                    if !self.swarm.behaviour_mut().transfer_proof.is_connected(&peer) {
                        tracing::warn!(%peer, "No active connection to peer, buffering transfer proof");
                        self.buffered_transfer_proofs.entry(peer).or_default().push((transfer_proof, responder));
                        continue;
                    }

                    // If we are connected to the peer, we attempt to send the transfer proof
                    let id = self.swarm.behaviour_mut().transfer_proof.send_request(&peer, transfer_proof);
                    self.inflight_transfer_proofs.insert(id, responder);
                },
                Some(response_channel) = self.inflight_encrypted_signatures.next() => {
                    let _ = self.swarm.behaviour_mut().encrypted_signature.send_response(response_channel, ());
                }
            }
        }
    }

    /// Emits the low-balance warning once per low-balance episode: quotes are
    /// recomputed on every seller-list poll, which otherwise repeats the same
    /// warning every few seconds. `make_quote` clears the flag once the balance
    /// is sufficient again, so a later drop warns once more.
    fn warn_low_balance_once(&mut self, message: String) {
        if self.low_balance_warned {
            return;
        }
        self.low_balance_warned = true;
        tracing::warn!("{}", message);
    }

    async fn make_quote(
        &mut self,
        min_buy: bitcoin::Amount,
        max_buy: bitcoin::Amount,
    ) -> Result<BidQuote> {
        let ask_price = self
            .latest_rate
            .latest_rate()
            .context("Failed to get latest rate")?
            .ask()
            .context("Failed to compute asking price")?;

        let balance = tokio::time::timeout(
            ASB_WALLET_RPC_TIMEOUT,
            self.monero_wallet.get_balance(),
        )
        .await
        .context("Timed out waiting for the Monero wallet balance")?
        .context("Failed to fetch the Monero wallet balance")?;

        // Use unlocked Monero balance minus active reservations and the fee
        // reserve for the new swap for the quote.
        let xmr_balance = Amount::from_piconero(
            balance
                .unlocked_balance
                .saturating_sub(self.liquidity.total().as_piconero())
                .saturating_sub(monero::MONERO_FEE.as_piconero()),
        );

        let max_bitcoin_for_monero =
            xmr_balance
                .max_bitcoin_for_price(ask_price)
                .ok_or_else(|| {
                    anyhow!(
                        "Bitcoin price ({}) x Monero ({}) overflow",
                        ask_price,
                        xmr_balance
                    )
                })?;

        tracing::debug!(
            %ask_price,
            %xmr_balance,
            reserved = %self.liquidity.total(),
            %max_bitcoin_for_monero,
            "Computed quote"
        );

        if min_buy > max_bitcoin_for_monero {
            self.warn_low_balance_once(format!(
                        "Your Monero balance is too low to initiate a swap, as your minimum swap amount is {}. You could at most swap {}",
                        min_buy, max_bitcoin_for_monero
                    ));

            return Ok(BidQuote {
                price: ask_price,
                min_quantity: bitcoin::Amount::ZERO,
                max_quantity: bitcoin::Amount::ZERO,
                refund_policy: RefundPolicyWire::FullRefund,
                reserve_proof: None,
            });
        }

        if max_buy > max_bitcoin_for_monero {
            self.warn_low_balance_once(format!(
                    "Your Monero balance is too low to initiate a swap with the maximum swap amount {} that you have specified in your config. You can at most swap {}",
                    max_buy, max_bitcoin_for_monero
                ));
            return Ok(BidQuote {
                price: ask_price,
                min_quantity: min_buy,
                max_quantity: max_bitcoin_for_monero,
                refund_policy: RefundPolicyWire::FullRefund,
                reserve_proof: None,
            });
        }

        self.low_balance_warned = false;

        Ok(BidQuote {
            price: ask_price,
            min_quantity: min_buy,
            max_quantity: max_buy,
            refund_policy: RefundPolicyWire::FullRefund,
            reserve_proof: None,
        })
    }

    async fn handle_execution_setup_done(
        &mut self,
        bob_peer_id: PeerId,
        swap_id: Uuid,
        state3: State3,
    ) {
        let handle = self.new_handle(bob_peer_id, swap_id);

        let initial_state = AliceState::Started {
            state3: Box::new(state3),
        };

        // Persist the setup state before starting the task. If the ASB dies
        // after Message4 but before the task persists its first transition,
        // the swap can still be resumed and its liquidity re-reserved.
        if let Err(error) = self
            .db
            .insert_latest_state(swap_id, initial_state.clone().into())
            .await
        {
            self.liquidity.release(swap_id);
            tracing::warn!(
                %swap_id,
                "Failed to persist the setup state; released its liquidity reservation: {}",
                error
            );
            return;
        }

        let swap = Swap {
            event_loop_handle: handle,
            bitcoin_wallet: self.bitcoin_wallet.clone(),
            monero_wallet: self.monero_wallet.clone(),
            env_config: self.env_config,
            db: self.db.clone(),
            state: initial_state,
            swap_id,
        };

        // The caller has already saved the peer id and rejected duplicate
        // swap ids before re-keying the reservation.
        if let Err(error) = self.swap_sender.send(swap).await {
            self.liquidity.release(swap_id);
            tracing::warn!(%swap_id, "Failed to start swap; released its liquidity reservation: {}", error);
        }
    }

    /// Create a new [`EventLoopHandle`] that is scoped for communication with
    /// the given peer.
    fn new_handle(&mut self, peer: PeerId, swap_id: Uuid) -> EventLoopHandle {
        // Create a new channel for receiving encrypted signatures from Bob
        // The channel has a capacity of 1 since we only expect one signature per swap
        let (encrypted_signature_sender, encrypted_signature_receiver) = bmrng::channel(1);

        // The sender is stored in the EventLoop
        // The receiver is stored in the EventLoopHandle
        // When a signature is received, the EventLoop uses the sender to notify the EventLoopHandle
        self.recv_encrypted_signature
            .insert(swap_id, encrypted_signature_sender);

        let transfer_proof_sender = self.outgoing_transfer_proofs_sender.clone();

        EventLoopHandle {
            swap_id,
            peer,
            recv_encrypted_signature: Some(encrypted_signature_receiver),
            transfer_proof_sender: Some(transfer_proof_sender),
        }
    }
}

pub trait LatestRate {
    type Error: std::error::Error + Send + Sync + 'static;

    fn latest_rate(&mut self) -> Result<Rate, Self::Error>;
}

#[derive(Clone, Debug)]
pub struct FixedRate(Rate);

impl FixedRate {
    pub const RATE: f64 = 0.01;

    /// Ask price taken verbatim from the operator's manual price (BTC per
    /// 1 XMR). No spread is applied: the configured price IS the quote.
    pub fn from_price_btc(price_btc: f64) -> Result<Self> {
        if !price_btc.is_finite() || price_btc <= 0.0 {
            anyhow::bail!("manual price must be a finite, positive BTC amount, got {price_btc}");
        }

        let ask = bitcoin::Amount::from_btc(price_btc)?;
        Ok(Self(Rate::new(ask, Decimal::from(0u64))))
    }

    pub fn value(&self) -> Rate {
        self.0
    }
}

impl Default for FixedRate {
    fn default() -> Self {
        let ask = bitcoin::Amount::from_btc(Self::RATE).expect("Static value should never fail");
        let spread = Decimal::from(0u64);

        Self(Rate::new(ask, spread))
    }
}

impl LatestRate for FixedRate {
    type Error = Infallible;

    fn latest_rate(&mut self) -> Result<Rate, Self::Error> {
        Ok(self.value())
    }
}

/// Produces [`Rate`]s based on [`PriceUpdate`]s from kraken and a configured
/// spread.
#[derive(Debug, Clone)]
pub struct KrakenRate {
    ask_spread: Decimal,
    price_updates: kraken::PriceUpdates,
}

impl KrakenRate {
    pub fn new(ask_spread: Decimal, price_updates: kraken::PriceUpdates) -> Self {
        Self {
            ask_spread,
            price_updates,
        }
    }
}

impl LatestRate for KrakenRate {
    type Error = kraken::Error;

    fn latest_rate(&mut self) -> Result<Rate, Self::Error> {
        let update = self.price_updates.latest_update()?;
        let rate = Rate::new(update.ask, self.ask_spread);

        Ok(rate)
    }
}

/// Rate source for the ASB: market-derived (ticker + spread) or a fixed
/// price manually configured by the operator.
#[derive(Clone, Debug)]
pub enum RateSource {
    Market(KrakenRate),
    Fixed(FixedRate),
}

#[derive(Debug, thiserror::Error)]
pub enum RateSourceError {
    #[error("market rate unavailable: {0}")]
    Market(#[from] kraken::Error),
}

impl LatestRate for RateSource {
    type Error = RateSourceError;

    fn latest_rate(&mut self) -> Result<Rate, Self::Error> {
        match self {
            RateSource::Market(rate) => rate.latest_rate().map_err(RateSourceError::from),
            RateSource::Fixed(rate) => Ok(rate.latest_rate().unwrap()),
        }
    }
}

#[derive(Debug)]
pub struct EventLoopHandle {
    swap_id: Uuid,
    peer: PeerId,
    recv_encrypted_signature: Option<bmrng::RequestReceiver<bitcoin::EncryptedSignature, ()>>,
    #[allow(clippy::type_complexity)]
    transfer_proof_sender: Option<
        tokio::sync::mpsc::UnboundedSender<(
            PeerId,
            transfer_proof::Request,
            oneshot::Sender<Result<(), OutboundFailure>>,
        )>,
    >,
}

impl EventLoopHandle {
    fn build_transfer_proof_request(
        &self,
        transfer_proof: monero::TransferProof,
    ) -> transfer_proof::Request {
        transfer_proof::Request {
            swap_id: self.swap_id,
            tx_lock_proof: transfer_proof,
        }
    }

    /// Wait for an encrypted signature from Bob
    pub async fn recv_encrypted_signature(&mut self) -> Result<bitcoin::EncryptedSignature> {
        let receiver = self
            .recv_encrypted_signature
            .as_mut()
            .context("Encrypted signature was already received")?;

        let (tx_redeem_encsig, responder) = receiver.recv().await?;

        // Acknowledge receipt of the encrypted signature
        // This notifies the EventLoop that the signature has been processed
        // The EventLoop can then send an acknowledgement back to Bob over the network
        responder
            .respond(())
            .context("Failed to acknowledge receipt of encrypted signature")?;

        // Only take after successful receipt and acknowledgement
        self.recv_encrypted_signature.take();

        Ok(tx_redeem_encsig)
    }

    /// Send a transfer proof to Bob
    ///
    /// This function will retry indefinitely until the transfer proof is sent successfully
    /// and acknowledged by Bob
    ///
    /// This will fail if
    /// 1. the transfer proof has already been sent once
    /// 2. there is an error with the bmrng channel
    pub async fn send_transfer_proof(&mut self, msg: monero::TransferProof) -> Result<()> {
        let sender = self
            .transfer_proof_sender
            .as_ref()
            .context("Transfer proof was already sent")?;

        // We will retry indefinitely until we succeed
        let backoff = backoff::ExponentialBackoffBuilder::new()
            .with_max_elapsed_time(None)
            .with_max_interval(Duration::from_secs(60))
            .build();

        let transfer_proof = self.build_transfer_proof_request(msg);

        backoff::future::retry(backoff, || async {
            // Create a oneshot channel to receive the acknowledgment of the transfer proof
            let (singular_sender, singular_receiver) = oneshot::channel();

            if let Err(err) = sender.send((self.peer, transfer_proof.clone(), singular_sender)) {
                let err = anyhow!(err).context("Failed to communicate transfer proof through event loop channel");
                tracing::error!(%err, swap_id = %self.swap_id, "Failed to send transfer proof");
                return Err(backoff::Error::permanent(err));
            }

            match singular_receiver.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(err)) => {
                    tracing::warn!(%err, "Failed to send transfer proof due to a network error. We will retry");
                    Err(backoff::Error::transient(anyhow!(err)))
                }
                Err(_) => {
                    Err(backoff::Error::permanent(anyhow!("The sender channel should never be closed without sending a response")))
                }
            }
        })
            .await?;

        self.transfer_proof_sender.take();

        Ok(())
    }
}

#[allow(missing_debug_implementations)]
struct MpscChannels<T> {
    sender: mpsc::Sender<T>,
    receiver: mpsc::Receiver<T>,
}

impl<T> Default for MpscChannels<T> {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel(100);
        MpscChannels { sender, receiver }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_rate_quotes_the_manual_price_verbatim_without_spread() {
        let rate = FixedRate::from_price_btc(0.007153).unwrap();

        assert_eq!(
            rate.value().ask().unwrap(),
            bitcoin::Amount::from_sat(715_300)
        );
    }

    #[test]
    fn quote_cache_serves_within_window_and_expires_after() {
        let quote = BidQuote {
            price: bitcoin::Amount::from_sat(1),
            min_quantity: bitcoin::Amount::from_sat(1),
            max_quantity: bitcoin::Amount::from_sat(1),
            refund_policy: RefundPolicyWire::FullRefund,
            reserve_proof: None,
        };
        let now = Instant::now();
        let cached = (now, quote.clone());

        assert_eq!(
            fresh_cached_quote(Some(&cached), now + Duration::from_millis(500)),
            Some(&quote)
        );
        assert_eq!(
            fresh_cached_quote(Some(&cached), now + Duration::from_secs(2)),
            None
        );
    }

    #[test]
    fn manual_price_rejects_invalid_values() {
        for invalid in [-0.5, 0.0, f64::NAN, f64::INFINITY] {
            assert!(
                FixedRate::from_price_btc(invalid).is_err(),
                "{invalid} must be rejected as a manual price"
            );
        }
    }

    #[test]
    fn fixed_rate_source_stays_stable_without_any_market_data() {
        let mut source = RateSource::Fixed(FixedRate::from_price_btc(0.007153).unwrap());

        let first = source.latest_rate().unwrap();
        let second = source.latest_rate().unwrap();

        assert_eq!(first, second);
        assert_eq!(first.ask().unwrap(), bitcoin::Amount::from_sat(715_300));
    }
}
