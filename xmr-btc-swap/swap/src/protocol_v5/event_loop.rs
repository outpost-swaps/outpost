use crate::protocol_v5::p2p::out_event;
use crate::protocol_v5::p2p::protocols::quote::{self, BidQuote};
use crate::protocol_v5::p2p::protocols::{encrypted_signature, transfer_proof};
use crate::protocol_v5::p2p::protocols::swap_setup::bob::{
    Behaviour, NewSwap, PendingCommit,
};
use crate::protocol_v5::{db, machine};
use crate::protocol_v5::store::ProtocolV5Store;
use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{PeerId, Swarm};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(libp2p::swarm::NetworkBehaviour)]
#[behaviour(to_swarm = "TakerEvent")]
pub struct TakerBehaviour {
    pub quote: quote::Behaviour,
    pub swap_setup: Behaviour,
    pub transfer_proof: transfer_proof::Behaviour,
    pub encrypted_signature: encrypted_signature::Behaviour,
}

#[derive(Debug)]
pub enum TakerEvent {
    Quote(libp2p::request_response::Event<(), BidQuote>),
    SwapSetup(crate::protocol_v5::p2p::protocols::swap_setup::bob::SwapSetupResult),
    TransferProof(libp2p::request_response::Event<transfer_proof::Request, ()>),
    EncryptedSignature(libp2p::request_response::Event<encrypted_signature::Request, ()>),
}

impl From<libp2p::request_response::Event<(), BidQuote>> for TakerEvent {
    fn from(event: libp2p::request_response::Event<(), BidQuote>) -> Self {
        Self::Quote(event)
    }
}

impl From<crate::protocol_v5::p2p::protocols::swap_setup::bob::SwapSetupResult> for TakerEvent {
    fn from(event: crate::protocol_v5::p2p::protocols::swap_setup::bob::SwapSetupResult) -> Self {
        Self::SwapSetup(event)
    }
}

impl From<libp2p::request_response::Event<transfer_proof::Request, ()>> for TakerEvent {
    fn from(event: libp2p::request_response::Event<transfer_proof::Request, ()>) -> Self {
        Self::TransferProof(event)
    }
}

impl From<libp2p::request_response::Event<encrypted_signature::Request, ()>> for TakerEvent {
    fn from(event: libp2p::request_response::Event<encrypted_signature::Request, ()>) -> Self {
        Self::EncryptedSignature(event)
    }
}

pub struct BobEventLoop {
    swarm: Swarm<TakerBehaviour>,
    alice: PeerId,
    store: Arc<dyn ProtocolV5Store>,
    new_swaps: bmrng::RequestReceiver<NewSwap, Result<machine::bob::State2>>,
    commits: bmrng::RequestReceiver<(), Result<()>>,
    quote_requests: bmrng::RequestReceiver<(), Result<BidQuote>>,
    quote_responder: Option<bmrng::Responder<Result<BidQuote>>>,
    transfer_proof_sender: mpsc::UnboundedSender<transfer_proof::Request>,
    transfer_proof_acks: bmrng::RequestReceiver<(), Result<()>>,
    pending_transfer_proof: Option<libp2p::request_response::ResponseChannel<()>>,
    encrypted_signature_requests: bmrng::RequestReceiver<encrypted_signature::Request, Result<()>>,
    encrypted_signature_responder: Option<bmrng::Responder<Result<()>>>,
    setup_responder: Option<bmrng::Responder<Result<machine::bob::State2>>>,
    pending: Option<PendingCommit>,
    commit_results: mpsc::UnboundedReceiver<Result<()>>,
    commit_result_sender: mpsc::UnboundedSender<Result<()>>,
    commit_responder: Option<bmrng::Responder<Result<()>>>,
    /// Seller dial address, kept so Bob can re-establish the connection while
    /// waiting for the transfer proof. Alice pushes the proof Outbound-only and
    /// buffers it until a connection exists; a dropped setup connection (long
    /// testnet confirmations) previously stranded that buffer and timed Bob out.
    seller_address: libp2p::Multiaddr,
    transfer_proof_received: bool,
    /// Ticks spent connected while still waiting for the proof. A half-dead
    /// connection can look connected locally while the maker cannot dial us;
    /// after five minutes force a fresh handshake.
    proof_wait_ticks: u32,
    redial_tick: tokio::time::Interval,
}

/// Build the live taker swarm with Outpost's Tor/DNS/websocket transport.
pub async fn new_bob_swarm(
    identity: libp2p::identity::Keypair,
    tor_socks5_port: u16,
    env_config: crate::protocol_v5::env::env::Config,
    bitcoin_wallet: Arc<dyn crate::protocol_v5::bitcoin_wallet::BitcoinWallet>,
) -> Result<Swarm<TakerBehaviour>> {
    crate::network::swarm::cli(
        identity,
        tor_socks5_port,
        TakerBehaviour {
            quote: quote::bob(),
            swap_setup: Behaviour::new(env_config, bitcoin_wallet),
            transfer_proof: transfer_proof::bob(),
            encrypted_signature: encrypted_signature::bob(),
        },
    )
    .await
}

pub struct BobEventLoopHandle {
    new_swaps: bmrng::RequestSender<NewSwap, Result<machine::bob::State2>>,
    commits: bmrng::RequestSender<(), Result<()>>,
    quote_requests: bmrng::RequestSender<(), Result<BidQuote>>,
    transfer_proofs: mpsc::UnboundedReceiver<transfer_proof::Request>,
    transfer_proof_acks: bmrng::RequestSender<(), Result<()>>,
    encrypted_signatures: bmrng::RequestSender<encrypted_signature::Request, Result<()>>,
    /// Alice sends the transfer proof once and drops duplicates, so a retry of
    /// the buy flow after a late failure must reuse the proof it already
    /// consumed instead of waiting for one that never comes again (audit
    /// first-review #1).
    latch: ProofLatch,
}

#[derive(Default)]
pub(crate) struct ProofLatch {
    proof: Option<transfer_proof::Request>,
    acked: bool,
    accepted: Option<machine::bob::State4>,
}

impl BobEventLoop {
    pub fn new(
        swarm: Swarm<TakerBehaviour>,
        alice: PeerId,
        store: Arc<dyn ProtocolV5Store>,
        seller_address: libp2p::Multiaddr,
    ) -> (Self, BobEventLoopHandle) {
        let (new_swaps_sender, new_swaps) = bmrng::channel(1);
        let (commits_sender, commits) = bmrng::channel(1);
        let (quote_sender, quote_requests) = bmrng::channel(1);
        let (transfer_proof_sender, transfer_proofs) = mpsc::unbounded_channel();
        let (transfer_proof_ack_sender, transfer_proof_acks) = bmrng::channel(1);
        let (encrypted_signature_sender, encrypted_signature_requests) = bmrng::channel(1);
        let (commit_result_sender, commit_results) = mpsc::unbounded_channel();

        let event_loop = Self {
            swarm,
            alice,
            store,
            new_swaps,
            commits,
            quote_requests,
            quote_responder: None,
            transfer_proof_sender,
            transfer_proof_acks,
            pending_transfer_proof: None,
            encrypted_signature_requests,
            encrypted_signature_responder: None,
            setup_responder: None,
            pending: None,
            commit_results,
            commit_result_sender,
            commit_responder: None,
            seller_address,
            transfer_proof_received: false,
            proof_wait_ticks: 0,
            redial_tick: tokio::time::interval(std::time::Duration::from_secs(5)),
        };

        let handle = BobEventLoopHandle {
            new_swaps: new_swaps_sender,
            commits: commits_sender,
            quote_requests: quote_sender,
            transfer_proofs,
            transfer_proof_acks: transfer_proof_ack_sender,
            encrypted_signatures: encrypted_signature_sender,
            latch: ProofLatch::default(),
        };

        (event_loop, handle)
    }

    pub async fn run(mut self) {
        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => {
                    // A failed dial only reaches the quote/proof handlers as a
                    // bare "Failed to dial the requested peer"; the transport's
                    // reason (refused, SOCKS failure, timeout) is in this event.
                    if let SwarmEvent::OutgoingConnectionError { peer_id, error, .. } = &event {
                        tracing::warn!(?peer_id, %error, "V5_BOB_DIAL_ERROR");
                    }
                    let SwarmEvent::Behaviour(result) = event else {
                        continue;
                    };
                    match result {
                        TakerEvent::Quote(event) => {
                            match event {
                                libp2p::request_response::Event::Message {
                                    message: libp2p::request_response::Message::Response {
                                        response,
                                        ..
                                    },
                                    ..
                                } => {
                                    tracing::info!("V5_BOB_QUOTE_RESPONSE");
                                    if let Some(responder) = self.quote_responder.take() {
                                        let _ = responder.respond(Ok(response));
                                    }
                                }
                                libp2p::request_response::Event::OutboundFailure { error, .. } => {
                                    tracing::warn!(%error, "V5_BOB_QUOTE_OUTBOUND_FAILURE");
                                    if let Some(responder) = self.quote_responder.take() {
                                        let _ = responder.respond(Err(anyhow!(
                                            "quote request failed: {error}"
                                        )));
                                    }
                                }
                                _ => {}
                            }
                        }
                        TakerEvent::TransferProof(event) => {
                            match event {
                                libp2p::request_response::Event::Message {
                                    message: libp2p::request_response::Message::Request {
                                        request, channel, ..
                                    },
                                    ..
                                } => {
                                    if self.transfer_proof_received {
                                        // Alice retries until her ack arrives. Once Bob
                                        // has received a proof, acknowledge duplicates
                                        // immediately instead of forwarding them to an
                                        // app state that may already have moved on.
                                        let _ = self
                                            .swarm
                                            .behaviour_mut()
                                            .transfer_proof
                                            .send_response(channel, ());
                                        continue;
                                    }
                                    self.transfer_proof_received = true;
                                    if self.transfer_proof_sender.send(request).is_err() {
                                        let _ = self
                                            .swarm
                                            .behaviour_mut()
                                            .transfer_proof
                                            .send_response(channel, ());
                                    } else {
                                        self.pending_transfer_proof = Some(channel);
                                    }
                                }
                                libp2p::request_response::Event::InboundFailure { error, .. } => {
                                    tracing::warn!("Failed to receive transfer proof: {error}");
                                }
                                _ => {}
                            }
                        }
                        TakerEvent::EncryptedSignature(event) => {
                            match event {
                                libp2p::request_response::Event::Message {
                                    message: libp2p::request_response::Message::Response { .. },
                                    ..
                                } => {
                                    if let Some(responder) = self.encrypted_signature_responder.take() {
                                        let _ = responder.respond(Ok(()));
                                    }
                                }
                                libp2p::request_response::Event::OutboundFailure { error, .. } => {
                                    if let Some(responder) = self.encrypted_signature_responder.take() {
                                        let _ = responder.respond(Err(anyhow!(
                                            "encrypted signature request failed: {error}"
                                        )));
                                    }
                                }
                                _ => {}
                            }
                        }
                        TakerEvent::SwapSetup(result) => {
                    let event: out_event::bob::OutEvent = result.into();

                    match event {
                        out_event::bob::OutEvent::SwapSetupCompleted { swap_id, result, .. } => {
                            match *result {
                                Ok((state2, pending)) => {
                                    let stored = db::bob::Bob::ExecutionSetupDone {
                                        state2: state2.clone(),
                                    };
                                    if let Err(error) = self.store.store_bob(swap_id, &stored).await {
                                        tracing::error!(%swap_id, "Failed to persist protocol-5 setup: {error:#}");
                                        if let Some(responder) = self.setup_responder.take() {
                                            let _ = responder.respond(Err(error));
                                        }
                                        continue;
                                    }

                                    self.pending = Some(pending);
                                    if let Some(responder) = self.setup_responder.take() {
                                        let _ = responder.respond(Ok(state2));
                                    }
                                }
                                Err(error) => {
                                    if let Some(responder) = self.setup_responder.take() {
                                        let _ = responder.respond(Err(error));
                                    }
                                }
                            }
                        }
                        out_event::bob::OutEvent::Failure { error, .. } => {
                            tracing::warn!("Protocol-5 swap setup failed: {error:#}");
                            if let Some(responder) = self.setup_responder.take() {
                                let _ = responder.respond(Err(error));
                            }
                        }
                        _ => {}
                    }
                        }
                    }
                }
                _ = self.redial_tick.tick(), if !self.transfer_proof_received => {
                    // The maker pushes the transfer proof Outbound-only and
                    // buffers it until a connection to us exists. The setup
                    // connection can drop during long confirmation waits, so
                    // keep re-dialing the seller until the proof arrives.
                    if self.swarm.is_connected(&self.alice) {
                        self.proof_wait_ticks += 1;
                        if self.proof_wait_ticks >= 60 {
                            tracing::warn!(
                                peer = %self.alice,
                                "No transfer proof after 5 minutes on an existing connection; forcing a fresh handshake"
                            );
                            let _ = self.swarm.disconnect_peer_id(self.alice);
                            self.proof_wait_ticks = 0;
                        }
                    } else {
                        self.proof_wait_ticks = 0;
                        match self.swarm.dial(self.seller_address.clone()) {
                            Ok(_) => tracing::debug!(
                                peer = %self.alice,
                                "Re-dialing seller while waiting for the transfer proof"
                            ),
                            Err(error) => tracing::warn!(
                                peer = %self.alice,
                                error = %error,
                                "Failed to re-dial seller for transfer proof delivery"
                            ),
                        }
                    }
                }
                request = self.quote_requests.recv() => {
                    let Ok(((), responder)) = request else {
                        break;
                    };
                    self.quote_responder = Some(responder);
                    self.swarm
                        .behaviour_mut()
                        .quote
                        .send_request(&self.alice, ());
                }
                request = self.transfer_proof_acks.recv() => {
                    let Ok(((), responder)) = request else {
                        break;
                    };
                    let Some(channel) = self.pending_transfer_proof.take() else {
                        let _ = responder.respond(Err(anyhow!("no transfer proof waiting for acknowledgement")));
                        continue;
                    };
                    let _ = self
                        .swarm
                        .behaviour_mut()
                        .transfer_proof
                        .send_response(channel, ());
                    let _ = responder.respond(Ok(()));
                }
                request = self.encrypted_signature_requests.recv() => {
                    let Ok((request, responder)) = request else {
                        break;
                    };
                    self.encrypted_signature_responder = Some(responder);
                    self.swarm
                        .behaviour_mut()
                        .encrypted_signature
                        .send_request(&self.alice, request);
                }
                request = self.new_swaps.recv() => {
                    let Ok((swap, responder)) = request else {
                        break;
                    };

                    if self.pending.is_some() || self.setup_responder.is_some() {
                        let _ = responder.respond(Err(anyhow!(
                            "another protocol-5 swap setup is already in progress"
                        )));
                        continue;
                    }

                    self.swarm.behaviour_mut().swap_setup.queue_new_swap(self.alice, swap);
                    self.setup_responder = Some(responder);
                }
                request = self.commits.recv() => {
                    let Ok(((), responder)) = request else {
                        break;
                    };

                    let Some(pending) = self.pending.take() else {
                        let _ = responder.respond(Err(anyhow!("no protocol-5 swap setup to commit")));
                        continue;
                    };

                    self.commit_responder = Some(responder);
                    let sender = self.commit_result_sender.clone();
                    tokio::spawn(async move {
                        let result = pending.commit().await;
                        let _ = sender.send(result);
                    });
                }
                result = self.commit_results.recv() => {
                    let Some(result) = result else {
                        break;
                    };
                    if let Some(responder) = self.commit_responder.take() {
                        let _ = responder.respond(result);
                    }
                }
            }
        }
    }
}

impl BobEventLoopHandle {
    pub async fn request_quote(&self) -> Result<BidQuote> {
        tracing::info!("V5_BOB_QUOTE_REQUEST_SENT");
        self.quote_requests
            .clone()
            .send_receive(())
            .await
            .context("protocol-5 event loop is not running")?
    }

    /// The transfer proof for this swap. The first call waits for Alice's
    /// message; later calls return the same proof.
    pub async fn recv_transfer_proof(&mut self) -> Result<transfer_proof::Request> {
        if let Some(proof) = &self.latch.proof {
            return Ok(proof.clone());
        }
        let proof = self
            .transfer_proofs
            .recv()
            .await
            .context("protocol-5 event loop stopped before receiving a transfer proof")?;
        self.latch.proof = Some(proof.clone());
        Ok(proof)
    }

    /// Acknowledge the proof to Alice once; repeat calls are no-ops.
    pub async fn ack_transfer_proof(&mut self) -> Result<()> {
        if self.latch.acked {
            return Ok(());
        }
        self.transfer_proof_acks
            .clone()
            .send_receive(())
            .await
            .context("protocol-5 event loop is not running")??;
        self.latch.acked = true;
        Ok(())
    }

    /// Remember that Alice's lock was verified and accepted, so callers can
    /// tell that a later failure happened with the redeem signature possibly
    /// already out.
    pub fn record_xmr_lock_accepted(&mut self, state4: machine::bob::State4) {
        self.latch.accepted = Some(state4);
    }

    pub fn accepted_state4(&self) -> Option<machine::bob::State4> {
        self.latch.accepted.clone()
    }

    pub async fn send_encrypted_signature(
        &self,
        request: encrypted_signature::Request,
    ) -> Result<()> {
        self.encrypted_signatures
            .clone()
            .send_receive(request)
            .await
            .context("protocol-5 event loop is not running")?
    }

    pub async fn start_swap(&self, swap: NewSwap) -> Result<machine::bob::State2> {
        self.new_swaps
            .clone()
            .send_receive(swap)
            .await
            .context("protocol-5 event loop is not running")?
    }

    pub async fn commit(&self) -> Result<()> {
        self.commits
            .clone()
            .send_receive(())
            .await
            .context("protocol-5 event loop is not running")?
    }
}

use futures::future;
use futures::FutureExt;
use uuid::Uuid;

use crate::protocol_v5::p2p::protocols::quote as quote_protocol;
use crate::protocol_v5::p2p::protocols::swap_setup::alice as alice_setup;

#[derive(libp2p::swarm::NetworkBehaviour)]
#[behaviour(to_swarm = "MakerEvent")]
pub struct MakerBehaviour<LR>
where
    LR: crate::protocol_v5::feed::LatestRate + Send + 'static + Clone + std::fmt::Debug,
{
    connection_limits: libp2p::connection_limits::Behaviour,
    pub rendezvous: libp2p::swarm::behaviour::toggle::Toggle<
        crate::protocol_v5::p2p::protocols::rendezvous::register::Behaviour,
    >,
    pub quote: quote_protocol::Behaviour,
    pub swap_setup: alice_setup::Behaviour<LR>,
    pub transfer_proof: transfer_proof::Behaviour,
    pub encrypted_signature: encrypted_signature::Behaviour,
    pub identify: crate::protocol_v5::p2p::patches::identify::Behaviour,
    ping: libp2p::ping::Behaviour,
}

impl<LR> MakerBehaviour<LR>
where
    LR: crate::protocol_v5::feed::LatestRate + Send + 'static + Clone + std::fmt::Debug,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        min_buy: crate::protocol_v5::core::bitcoin::Amount,
        max_buy: crate::protocol_v5::core::bitcoin::Amount,
        env_config: crate::protocol_v5::env::env::Config,
        latest_rate: LR,
        resume_only: bool,
        lock_input_verifier: crate::protocol_v5::p2p::protocols::swap_setup::verification::LockInputVerifier,
        identity: libp2p::identity::Keypair,
        rendezvous_nodes: Vec<libp2p::PeerId>,
        namespace: crate::protocol_v5::p2p::protocols::rendezvous::XmrBtcNamespace,
        connection_limits: libp2p::connection_limits::ConnectionLimits,
    ) -> Self {
        let namespace_string = namespace.to_string();
        let agent_version = asb_agent_version(&namespace_string);
        let identify = libp2p::identify::Config::new(
            "/comit/xmr/btc/1.0.0".to_string(),
            identity.public(),
        )
        .with_agent_version(agent_version);
        let rendezvous = if rendezvous_nodes.is_empty() {
            None
        } else {
            Some(
                crate::protocol_v5::p2p::protocols::rendezvous::register::Behaviour::new(
                    identity,
                    rendezvous_nodes,
                    namespace.into(),
                ),
            )
        };

        Self {
            connection_limits: libp2p::connection_limits::Behaviour::new(connection_limits),
            rendezvous: libp2p::swarm::behaviour::toggle::Toggle::from(rendezvous),
            quote: quote_protocol::alice(None),
            swap_setup: alice_setup::Behaviour::new(
                min_buy,
                max_buy,
                env_config,
                latest_rate,
                resume_only,
                lock_input_verifier,
            ),
            transfer_proof: transfer_proof::alice(None),
            encrypted_signature: encrypted_signature::alice(None),
            identify: crate::protocol_v5::p2p::patches::identify::Behaviour::new(identify),
            ping: libp2p::ping::Behaviour::new(
                libp2p::ping::Config::new().with_timeout(std::time::Duration::from_secs(60)),
            ),
        }
    }
}

#[derive(Debug)]
pub enum MakerEvent {
    Quote(libp2p::request_response::Event<(), BidQuote>),
    SwapSetup(alice_setup::OutEvent),
    TransferProof(libp2p::request_response::Event<transfer_proof::Request, ()>),
    EncryptedSignature(libp2p::request_response::Event<encrypted_signature::Request, ()>),
    Rendezvous(crate::protocol_v5::p2p::protocols::rendezvous::register::Event),
    Other,
}

impl From<libp2p::request_response::Event<(), BidQuote>> for MakerEvent {
    fn from(event: libp2p::request_response::Event<(), BidQuote>) -> Self {
        Self::Quote(event)
    }
}

impl From<alice_setup::OutEvent> for MakerEvent {
    fn from(event: alice_setup::OutEvent) -> Self {
        Self::SwapSetup(event)
    }
}

impl From<libp2p::request_response::Event<transfer_proof::Request, ()>> for MakerEvent {
    fn from(event: libp2p::request_response::Event<transfer_proof::Request, ()>) -> Self {
        Self::TransferProof(event)
    }
}

impl From<libp2p::request_response::Event<encrypted_signature::Request, ()>> for MakerEvent {
    fn from(event: libp2p::request_response::Event<encrypted_signature::Request, ()>) -> Self {
        Self::EncryptedSignature(event)
    }
}

impl From<crate::protocol_v5::p2p::protocols::rendezvous::register::Event> for MakerEvent {
    fn from(event: crate::protocol_v5::p2p::protocols::rendezvous::register::Event) -> Self {
        Self::Rendezvous(event)
    }
}

impl From<libp2p::ping::Event> for MakerEvent {
    fn from(_: libp2p::ping::Event) -> Self {
        Self::Other
    }
}

impl From<libp2p::identify::Event> for MakerEvent {
    fn from(_: libp2p::identify::Event) -> Self {
        Self::Other
    }
}

impl From<void::Void> for MakerEvent {
    fn from(event: void::Void) -> Self {
        void::unreachable(event)
    }
}

pub async fn new_maker_swarm<LR>(
    identity: libp2p::identity::Keypair,
    tor_socks5_port: u16,
    behaviour: MakerBehaviour<LR>,
) -> Result<Swarm<MakerBehaviour<LR>>>
where
    LR: crate::protocol_v5::feed::LatestRate + Send + 'static + Clone + std::fmt::Debug,
{
    crate::network::swarm::cli(identity, tor_socks5_port, behaviour).await
}

struct OutgoingTransferProof {
    peer: PeerId,
    request: transfer_proof::Request,
    responder: tokio::sync::oneshot::Sender<Result<()>>,
}

/// Keeps only the buffered items that `is_waiting` still accepts.
fn retain_waiting<T>(items: &mut Vec<T>, is_waiting: impl Fn(&T) -> bool) {
    items.retain(|item| is_waiting(item));
}

pub struct RegisteredSwap {
    pub swap_id: Uuid,
    pub state3: machine::alice::State3,
    pub lock_input_reservation: alice_setup::LockInputReservation,
    handle: AliceSwapHandle,
}

impl RegisteredSwap {
    pub fn handle(&mut self) -> &mut AliceSwapHandle {
        &mut self.handle
    }
}

pub struct AliceSwapHandle {
    pub swap_id: Uuid,
    pub peer_id: PeerId,
    outgoing_transfer_proofs: mpsc::UnboundedSender<OutgoingTransferProof>,
    encrypted_signatures: bmrng::RequestReceiver<encrypted_signature::Request, ()>,
    pending_ack: DeferredAck,
}

/// Holds Bob's acknowledgement until the maker has durably stored the state the
/// encrypted signature leads to. Bob treats an ack as "delivered" and never
/// resends, so acking earlier loses the signature if the maker crashes or the
/// signature turns out to be invalid (audit A-3).
#[derive(Default)]
pub(crate) struct DeferredAck(Option<bmrng::Responder<()>>);

impl DeferredAck {
    pub(crate) fn hold(&mut self, responder: bmrng::Responder<()>) {
        self.0 = Some(responder);
    }

    /// Acknowledge a held request. Returns whether there was one.
    pub(crate) fn release(&mut self) -> Result<bool> {
        match self.0.take() {
            Some(responder) => {
                responder
                    .respond(())
                    .context("protocol-5 encrypted signature acknowledgment channel closed")?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// What the maker event loop does with an inbound encrypted-signature request.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EncSigAction {
    /// Hand it to the swap task; ack once the swap has stored the result.
    Relay,
    /// Already stored earlier; Bob just missed our ack.
    AckAlreadyProcessed,
    /// No task is listening (not resumed yet, or it died). Do not respond: an
    /// unanswered request fails on Bob's side and he resends.
    LeaveUnanswered,
    /// A swap task is listening, but this peer is not the taker of the swap: ignore it and
    /// keep the channel for the real taker.
    WrongPeer,
}

pub(crate) fn encsig_action(
    registered_peer: Option<PeerId>,
    from: PeerId,
    already_acked: bool,
) -> EncSigAction {
    if let Some(expected) = registered_peer {
        if expected == from {
            EncSigAction::Relay
        } else {
            EncSigAction::WrongPeer
        }
    } else if already_acked {
        EncSigAction::AckAlreadyProcessed
    } else {
        EncSigAction::LeaveUnanswered
    }
}

impl AliceSwapHandle {
    pub async fn send_transfer_proof(
        &self,
        proof: crate::protocol_v5::core::monero::TransferProof,
    ) -> Result<()> {
        let (responder, response) = tokio::sync::oneshot::channel();
        self.outgoing_transfer_proofs
            .send(OutgoingTransferProof {
                peer: self.peer_id,
                request: transfer_proof::Request {
                    swap_id: self.swap_id,
                    tx_lock_proof: proof,
                },
                responder,
            })
            .map_err(|_| anyhow!("protocol-5 maker event loop stopped"))?;
        response
            .await
            .context("protocol-5 maker event loop stopped")?
    }

    pub async fn recv_encrypted_signature(&mut self) -> Result<encrypted_signature::Request> {
        let (request, responder) = self
            .encrypted_signatures
            .recv()
            .await
            .context("protocol-5 maker event loop stopped")?;
        // Not acknowledged here: the caller acks with `ack_encrypted_signature`
        // after the resulting state is stored.
        self.pending_ack.hold(responder);
        Ok(request)
    }

    /// Acknowledge a received encrypted signature to Bob. Call only after the
    /// state it produced has been persisted.
    pub fn ack_encrypted_signature(&mut self) -> Result<bool> {
        self.pending_ack.release()
    }
}

/// Most quote computations the maker runs at once.
const MAX_CONCURRENT_QUOTES: usize = 32;

#[async_trait::async_trait]
pub trait MakerQuoteProvider: Send + Sync {
    async fn quote(&self) -> Result<BidQuote>;
}

pub struct MakerSnapshot {
    pub snapshot: alice_setup::WalletSnapshot,
    pub anti_spam_deposit: crate::protocol_v5::core::bitcoin::Amount,
    pub hermes_enabled: bool,
}

#[async_trait::async_trait]
pub trait MakerWalletSnapshotProvider: Send + Sync {
    async fn snapshot(
        &self,
        btc_amount: crate::protocol_v5::core::bitcoin::Amount,
    ) -> Result<MakerSnapshot>;
    async fn reserve_for_swap(
        &self,
        swap_id: Uuid,
        btc_amount: crate::protocol_v5::core::bitcoin::Amount,
        xmr_amount: crate::protocol_v5::core::monero::Amount,
    ) -> Result<bool>;
    async fn release_liquidity(&self, swap_id: Uuid);
}

pub struct MakerEventLoop<LR>
where
    LR: crate::protocol_v5::feed::LatestRate + Send + 'static + Clone + std::fmt::Debug,
{
    swarm: Swarm<MakerBehaviour<LR>>,
    store: Arc<dyn ProtocolV5Store>,
    quote: Arc<dyn MakerQuoteProvider>,
    quote_responses: mpsc::UnboundedReceiver<(libp2p::request_response::ResponseChannel<BidQuote>, Result<BidQuote>)>,
    quote_responses_sender: mpsc::UnboundedSender<(libp2p::request_response::ResponseChannel<BidQuote>, Result<BidQuote>)>,
    /// Bounds quote computations in flight; each request would otherwise spawn an unbounded task (audit A-6).
    quote_permits: Arc<tokio::sync::Semaphore>,
    wallet_snapshot: Arc<dyn MakerWalletSnapshotProvider>,
    registered: mpsc::UnboundedSender<RegisteredSwap>,
    resumptions: bmrng::RequestReceiver<ResumeAliceSwap, Result<RegisteredSwap>>,
    outgoing_transfer_proofs: mpsc::UnboundedReceiver<OutgoingTransferProof>,
    outgoing_transfer_proofs_sender: mpsc::UnboundedSender<OutgoingTransferProof>,
    inflight_transfer_proofs: std::collections::HashMap<
        libp2p::request_response::OutboundRequestId,
        tokio::sync::oneshot::Sender<Result<()>>,
    >,
    /// Proofs whose peer is not connected yet. The taker swarm is dial-only,
    /// so after a dropped connection the maker has nothing to dial; hold the
    /// proof until the taker reconnects instead of failing every retry.
    pending_transfer_proofs: Vec<OutgoingTransferProof>,
    /// The taker each swap's signature must come from, with the channel to its swap task.
    encrypted_signature_senders:
        std::collections::HashMap<Uuid, (PeerId, bmrng::RequestSender<encrypted_signature::Request, ()>)>,
    encrypted_signature_sender_registrations: mpsc::UnboundedReceiver<(
        Uuid,
        PeerId,
        bmrng::RequestSender<encrypted_signature::Request, ()>,
    )>,
    encrypted_signature_sender_registrations_tx: mpsc::UnboundedSender<(
        Uuid,
        PeerId,
        bmrng::RequestSender<encrypted_signature::Request, ()>,
    )>,
    inflight_encrypted_signatures: futures::stream::FuturesUnordered<
        futures::future::BoxFuture<
            'static,
            (
                Uuid,
                Option<libp2p::request_response::ResponseChannel<()>>,
            ),
        >,
    >,
    /// Swaps whose encrypted signature was stored and acknowledged, so a resend
    /// (our ack was lost in transit) can be acknowledged again.
    acked_encrypted_signatures: std::collections::HashSet<Uuid>,
}

pub struct ResumeAliceSwap {
    pub swap_id: Uuid,
    pub peer_id: PeerId,
    pub state3: machine::alice::State3,
}

pub struct MakerEventLoopHandle {
    registered: mpsc::UnboundedReceiver<RegisteredSwap>,
    resumptions: bmrng::RequestSender<ResumeAliceSwap, Result<RegisteredSwap>>,
}

impl MakerEventLoopHandle {
    pub async fn next_swap(&mut self) -> Result<RegisteredSwap> {
        self.registered
            .recv()
            .await
            .context("protocol-5 maker event loop stopped")
    }

    pub async fn resume_swap(&mut self, request: ResumeAliceSwap) -> Result<RegisteredSwap> {
        self.resumptions
            .clone()
            .send_receive(request)
            .await
            .map_err(|error| anyhow!("protocol-5 maker event loop stopped: {error}"))?
    }
}

impl<LR> MakerEventLoop<LR>
where
    LR: crate::protocol_v5::feed::LatestRate + Send + 'static + Clone + std::fmt::Debug,
{
    pub fn new(
        swarm: Swarm<MakerBehaviour<LR>>,
        store: Arc<dyn ProtocolV5Store>,
        quote: Arc<dyn MakerQuoteProvider>,
        wallet_snapshot: Arc<dyn MakerWalletSnapshotProvider>,
    ) -> (Self, MakerEventLoopHandle) {
        let (registered_sender, registered) = mpsc::unbounded_channel();
        let (outgoing_transfer_proofs_sender, outgoing_transfer_proofs) = mpsc::unbounded_channel();
        let (sender_registrations_tx, sender_registrations) = mpsc::unbounded_channel();
        let (quote_responses_sender, quote_responses) = mpsc::unbounded_channel();
        let (resumptions_sender, resumptions) = bmrng::channel(1);
        let handle = MakerEventLoopHandle {
            registered,
            resumptions: resumptions_sender,
        };
        let event_loop = Self {
            swarm,
            store,
            quote,
            quote_responses,
            quote_responses_sender,
            quote_permits: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_QUOTES)),
            wallet_snapshot,
            registered: registered_sender,
            resumptions,
            outgoing_transfer_proofs,
            outgoing_transfer_proofs_sender,
            inflight_transfer_proofs: Default::default(),
            pending_transfer_proofs: Vec::new(),
            encrypted_signature_senders: Default::default(),
            encrypted_signature_sender_registrations: sender_registrations,
            encrypted_signature_sender_registrations_tx: sender_registrations_tx,
            inflight_encrypted_signatures: Default::default(),
            acked_encrypted_signatures: Default::default(),
        };
        (event_loop, handle)
    }

    pub fn peer_id(&self) -> PeerId {
        *libp2p::Swarm::local_peer_id(&self.swarm)
    }

    pub async fn run(mut self) {
        self.inflight_encrypted_signatures
            .push(future::pending().boxed());

        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => {
                    if let SwarmEvent::ConnectionEstablished { peer_id, .. } = event {
                        self.flush_pending_transfer_proofs(&peer_id);
                        continue;
                    }
                    let SwarmEvent::Behaviour(event) = event else {
                        continue;
                    };
                    match event {
                        MakerEvent::Quote(event) => {
                            if let libp2p::request_response::Event::Message {
                                message: libp2p::request_response::Message::Request { request: (), channel, .. },
                                ..
                            } = event {
                                // Over the limit the channel is dropped, which the peer sees as a failed request.
                                match self.quote_permits.clone().try_acquire_owned() {
                                    Ok(permit) => {
                                        let provider = self.quote.clone();
                                        let responses = self.quote_responses_sender.clone();
                                        tokio::spawn(async move {
                                            let quote = provider.quote().await;
                                            drop(permit);
                                            let _ = responses.send((channel, quote));
                                        });
                                    }
                                    Err(_) => tracing::warn!(
                                        limit = MAX_CONCURRENT_QUOTES,
                                        "Protocol-5 maker dropped a quote request: too many in flight"
                                    ),
                                }
                            }
                        }
                        MakerEvent::TransferProof(event) => match event {
                            libp2p::request_response::Event::Message {
                                message: libp2p::request_response::Message::Response { request_id, .. },
                                ..
                            } => {
                                if let Some(responder) = self.inflight_transfer_proofs.remove(&request_id) {
                                    let _ = responder.send(Ok(()));
                                }
                            }
                            libp2p::request_response::Event::OutboundFailure { request_id, error, .. } => {
                                if let Some(responder) = self.inflight_transfer_proofs.remove(&request_id) {
                                    let _ = responder.send(Err(anyhow!("transfer proof request failed: {error}")));
                                }
                            }
                            libp2p::request_response::Event::InboundFailure { error, .. } => {
                                tracing::warn!("Protocol-5 maker failed to receive transfer-proof ack: {error}");
                            }
                            _ => {}
                        },
                        MakerEvent::EncryptedSignature(event) => match event {
                            libp2p::request_response::Event::Message {
                                peer,
                                message: libp2p::request_response::Message::Request { request, channel, .. },
                                ..
                            } => {
                                let swap_id = request.swap_id;
                                // Decide before taking the sender out: a request from the wrong
                                // peer must not consume the one-shot channel (audit A-3, #3).
                                let action = encsig_action(
                                    self.encrypted_signature_senders.get(&swap_id).map(|(expected, _)| *expected),
                                    peer,
                                    self.acked_encrypted_signatures.contains(&swap_id),
                                );
                                match action {
                                    EncSigAction::Relay => {
                                        let (_, sender) = self
                                            .encrypted_signature_senders
                                            .remove(&swap_id)
                                            .expect("Relay implies a registered sender");
                                        let ack = async move {
                                            // Resolves when the swap task has stored the
                                            // resulting state and acked; an error means it
                                            // gave up, so Bob must not be told it arrived.
                                            let stored = sender.send_receive(request).await.is_ok();
                                            (swap_id, stored.then_some(channel))
                                        }.boxed();
                                        self.inflight_encrypted_signatures.push(ack);
                                    }
                                    EncSigAction::AckAlreadyProcessed => {
                                        let _ = self.swarm.behaviour_mut().encrypted_signature.send_response(channel, ());
                                    }
                                    EncSigAction::LeaveUnanswered => {
                                        tracing::warn!(
                                            %swap_id,
                                            "No swap task is listening for this encrypted signature; leaving it unanswered so Bob resends"
                                        );
                                    }
                                    EncSigAction::WrongPeer => {
                                        tracing::warn!(
                                            %swap_id,
                                            %peer,
                                            "Ignoring an encrypted signature from a peer that is not this swap's taker"
                                        );
                                    }
                                }
                            }
                            libp2p::request_response::Event::InboundFailure { error, .. } => {
                                tracing::warn!("Protocol-5 maker failed to receive encrypted signature: {error}");
                            }
                            _ => {}
                        },
                        MakerEvent::SwapSetup(event) => self.handle_setup_event(event),
                        MakerEvent::Rendezvous(event) => {
                            tracing::debug!("Protocol-5 maker rendezvous event: {event:?}");
                        }
                        MakerEvent::Other => {}
                    }
                }
                Some(outgoing) = self.outgoing_transfer_proofs.recv() => {
                    if self.swarm.is_connected(&outgoing.peer) {
                        let request_id = self.swarm.behaviour_mut().transfer_proof.send_request(&outgoing.peer, outgoing.request);
                        self.inflight_transfer_proofs.insert(request_id, outgoing.responder);
                    } else {
                        tracing::warn!(
                            peer = %outgoing.peer,
                            "Peer not connected; buffering the transfer proof until it reconnects"
                        );
                        // Each retry of a swap step buffers another copy; drop the ones whose
                        // swap task has stopped waiting so they do not pile up (audit A-7).
                        retain_waiting(&mut self.pending_transfer_proofs, |buffered| {
                            !buffered.responder.is_closed()
                        });
                        self.pending_transfer_proofs.push(outgoing);
                    }
                }
                Some((channel, quote)) = self.quote_responses.recv() => {
                    match quote {
                        Ok(quote) => {
                            let _ = self.swarm.behaviour_mut().quote.send_response(channel, quote);
                        }
                        Err(error) => {
                            tracing::warn!("Protocol-5 maker failed to compute a quote: {error:#}");
                        }
                    }
                }
                Ok((request, responder)) = self.resumptions.recv() => {
                    let (encrypted_signatures, encrypted_signatures_rx) = bmrng::channel(1);
                    let _ = self
                        .encrypted_signature_sender_registrations_tx
                        .send((request.swap_id, request.peer_id, encrypted_signatures));
                    let (release_sender, _release_receiver) = mpsc::unbounded_channel();
                    let result = RegisteredSwap {
                        swap_id: request.swap_id,
                        state3: request.state3.clone(),
                        lock_input_reservation: alice_setup::LockInputReservation::new(
                            Vec::new(),
                            release_sender,
                        ),
                        handle: AliceSwapHandle {
                            swap_id: request.swap_id,
                            peer_id: request.peer_id,
                            outgoing_transfer_proofs: self.outgoing_transfer_proofs_sender.clone(),
                            encrypted_signatures: encrypted_signatures_rx,
                            pending_ack: DeferredAck::default(),
                        },
                    };
                    let _ = responder.respond(Ok(result));
                }
                Some((swap_id, peer, sender)) = self.encrypted_signature_sender_registrations.recv() => {
                    self.encrypted_signature_senders.insert(swap_id, (peer, sender));
                }
                (swap_id, channel) = self.inflight_encrypted_signatures.select_next_some() => {
                    if let Some(channel) = channel {
                        self.acked_encrypted_signatures.insert(swap_id);
                        let _ = self.swarm.behaviour_mut().encrypted_signature.send_response(channel, ());
                    }
                }
            }
        }
    }

    fn flush_pending_transfer_proofs(&mut self, peer: &PeerId) {
        // A reconnect must not replay proofs for attempts nobody waits on any more.
        retain_waiting(&mut self.pending_transfer_proofs, |buffered| {
            !buffered.responder.is_closed()
        });
        let mut remaining = Vec::new();
        for outgoing in self.pending_transfer_proofs.drain(..) {
            if &outgoing.peer == peer && self.swarm.is_connected(peer) {
                tracing::info!(%peer, "Peer reconnected; sending the buffered transfer proof");
                let request_id = self
                    .swarm
                    .behaviour_mut()
                    .transfer_proof
                    .send_request(&outgoing.peer, outgoing.request);
                self.inflight_transfer_proofs.insert(request_id, outgoing.responder);
            } else {
                remaining.push(outgoing);
            }
        }
        self.pending_transfer_proofs = remaining;
    }

    fn handle_setup_event(&mut self, event: alice_setup::OutEvent) {
        match event {
            alice_setup::OutEvent::Initiated {
                send_wallet_snapshot,
                check_lock_inputs,
                register_swap,
                ..
            } => {
                let store = self.store.clone();
                let snapshot_provider = self.wallet_snapshot.clone();
                let registered = self.registered.clone();
                let outgoing = self.outgoing_transfer_proofs_sender.clone();
                let sender_registrations = self.encrypted_signature_sender_registrations_tx.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_setup(
                        send_wallet_snapshot,
                        check_lock_inputs,
                        register_swap,
                        store,
                        snapshot_provider,
                        registered,
                        outgoing,
                        sender_registrations,
                    )
                    .await
                    {
                        tracing::warn!("Protocol-5 maker setup failed: {error:#}");
                    }
                });
            }
            alice_setup::OutEvent::Error { peer_id, error } => {
                tracing::warn!(%peer_id, "Protocol-5 maker setup error: {error:#}");
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_setup(
    mut send_wallet_snapshot: bmrng::RequestReceiver<
        crate::protocol_v5::core::bitcoin::Amount,
        (
            alice_setup::WalletSnapshot,
            crate::protocol_v5::core::bitcoin::Amount,
            bool,
        ),
    >,
    mut check_lock_inputs: bmrng::RequestReceiver<
        alice_setup::LockInputCheck,
        Result<alice_setup::LockInputReservation, alice_setup::LockInputCheckError>,
    >,
    mut register_swap: bmrng::RequestReceiver<alice_setup::RegisterSwap, Result<()>>,
    store: Arc<dyn ProtocolV5Store>,
    snapshot_provider: Arc<dyn MakerWalletSnapshotProvider>,
    registered: mpsc::UnboundedSender<RegisteredSwap>,
    outgoing: mpsc::UnboundedSender<OutgoingTransferProof>,
    sender_registrations: mpsc::UnboundedSender<(
        Uuid,
        PeerId,
        bmrng::RequestSender<encrypted_signature::Request, ()>,
    )>,
) -> Result<()> {
    let (amount, responder) = send_wallet_snapshot.recv().await?;
    let maker_snapshot = snapshot_provider.snapshot(amount).await?;
    responder.respond((
        maker_snapshot.snapshot,
        maker_snapshot.anti_spam_deposit,
        maker_snapshot.hermes_enabled,
    ))?;

    let (check, responder) = check_lock_inputs.recv().await?;
    let outpoints: Vec<::bitcoin::OutPoint> = check
        .psbt
        .unsigned_tx
        .input
        .iter()
        .map(|input| input.previous_output)
        .collect();
    let reservation_id = Uuid::new_v4();
    if !store.reserve_lock_inputs(reservation_id, &outpoints).await? {
        let _ = responder.respond(Err(alice_setup::LockInputCheckError(
            "Lock transaction inputs are already used by another swap".to_string(),
        )));
        return Ok(());
    }
    let (release_sender, mut release_receiver) = mpsc::unbounded_channel();
    let release_store = Arc::clone(&store);
    let release_reservation_id = reservation_id;
    let release_swap_id = Arc::new(Mutex::new(None::<Uuid>));
    let release_swap_id_task = Arc::clone(&release_swap_id);
    tokio::spawn(async move {
        if release_receiver.recv().await.is_some() {
            let swap_id = *release_swap_id_task.lock().unwrap();
            let result = match swap_id {
                Some(swap_id) => release_store.release_lock_inputs(swap_id).await,
                None => {
                    release_store
                        .release_lock_input_reservation(release_reservation_id)
                        .await
                }
            };
            if let Err(error) = result {
                tracing::warn!(
                    %error,
                    %release_reservation_id,
                    ?swap_id,
                    "Failed to release the lock-input reservation"
                );
            }
        }
    });
    responder.respond(Ok(alice_setup::LockInputReservation::new(
        outpoints,
        release_sender,
    )))?;

    let (register, responder) = register_swap.recv().await?;
    store
        .rekey_lock_inputs(reservation_id, register.swap_id)
        .await?;
    *release_swap_id.lock().unwrap() = Some(register.swap_id);
    store
        .store_alice_peer_id(register.swap_id, &register.peer_id.to_string())
        .await?;
    if !snapshot_provider
        .reserve_for_swap(
            register.swap_id,
            register.state3.btc,
            register.state3.xmr,
        )
        .await?
    {
        store.release_lock_inputs(register.swap_id).await?;
        let _ = responder.respond(Err(anyhow!(
            "Not enough unlocked Monero liquidity for this swap"
        )));
        return Ok(());
    }
    // Everything from here until the swap is handed to the maker driver can
    // fail. The XMR reserved above must not stay reserved when it does (audit
    // A-5, first review #12); once the driver has the swap it releases at the
    // terminal state itself.
    let swap_id = register.swap_id;
    let handed_over = release_liquidity_on_error(
        &*snapshot_provider,
        swap_id,
        async {
            store
                .store_alice(
                    register.swap_id,
                    &db::alice::Alice::Started {
                        state3: register.state3.clone(),
                    },
                )
                .await?;

            let (encrypted_signatures, encrypted_signatures_rx) = bmrng::channel(1);
            sender_registrations
                .send((register.swap_id, register.peer_id, encrypted_signatures))
                .map_err(|_| anyhow!("protocol-5 maker event loop stopped"))?;
            if registered
                .send(RegisteredSwap {
                    swap_id: register.swap_id,
                    state3: register.state3.clone(),
                    lock_input_reservation: register.lock_input_reservation,
                    handle: AliceSwapHandle {
                        swap_id: register.swap_id,
                        peer_id: register.peer_id,
                        outgoing_transfer_proofs: outgoing,
                        encrypted_signatures: encrypted_signatures_rx,
                        pending_ack: DeferredAck::default(),
                    },
                })
                .is_err()
            {
                store.release_lock_inputs(register.swap_id).await?;
                anyhow::bail!("protocol-5 maker swap consumer stopped");
            }
            Ok(())
        },
    )
    .await;
    handed_over?;
    responder.respond(Ok(()))?;
    Ok(())
}

/// Runs `step`; if it fails, gives back the XMR reserved for `swap_id` before
/// returning the error.
async fn release_liquidity_on_error<T>(
    provider: &dyn MakerWalletSnapshotProvider,
    swap_id: Uuid,
    step: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let result = step.await;
    if result.is_err() {
        provider.release_liquidity(swap_id).await;
    }
    result
}

#[cfg(test)]
mod liquidity_release_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingProvider {
        released: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl MakerWalletSnapshotProvider for CountingProvider {
        async fn snapshot(
            &self,
            _btc_amount: crate::protocol_v5::core::bitcoin::Amount,
        ) -> Result<MakerSnapshot> {
            unimplemented!("not used")
        }
        async fn reserve_for_swap(
            &self,
            _swap_id: Uuid,
            _btc_amount: crate::protocol_v5::core::bitcoin::Amount,
            _xmr_amount: crate::protocol_v5::core::monero::Amount,
        ) -> Result<bool> {
            unimplemented!("not used")
        }
        async fn release_liquidity(&self, _swap_id: Uuid) {
            self.released.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn a_failed_step_releases_the_reservation() {
        let provider = CountingProvider { released: AtomicUsize::new(0) };
        let result: Result<()> =
            release_liquidity_on_error(&provider, Uuid::new_v4(), async { Err(anyhow!("store failed")) }).await;
        assert!(result.is_err());
        assert_eq!(provider.released.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_successful_step_keeps_the_reservation() {
        let provider = CountingProvider { released: AtomicUsize::new(0) };
        let result = release_liquidity_on_error(&provider, Uuid::new_v4(), async { Ok(5u8) }).await;
        assert_eq!(result.unwrap(), 5);
        assert_eq!(provider.released.load(Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
mod encsig_ack_tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_received_request_is_not_acknowledged_until_released() {
        let (tx, mut rx) = bmrng::channel::<u8, ()>(1);
        let mut ack = DeferredAck::default();

        let sender = tokio::spawn(async move { tx.send_receive(7).await });
        let (request, responder) = rx.recv().await.unwrap();
        assert_eq!(request, 7);
        ack.hold(responder);

        // Held: the requester is still waiting for its acknowledgement.
        let mut sender = sender;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut sender)
                .await
                .is_err(),
            "must not ack before the state is stored"
        );

        assert!(ack.release().unwrap());
        assert!(sender.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn dropping_the_held_ack_tells_the_requester_it_was_not_stored() {
        let (tx, mut rx) = bmrng::channel::<u8, ()>(1);
        let mut ack = DeferredAck::default();
        let sender = tokio::spawn(async move { tx.send_receive(1).await });
        let (_, responder) = rx.recv().await.unwrap();
        ack.hold(responder);
        drop(ack);
        assert!(sender.await.unwrap().is_err());
    }

    #[test]
    fn releasing_with_nothing_held_is_a_no_op() {
        assert!(!DeferredAck::default().release().unwrap());
    }

    fn peer() -> PeerId {
        PeerId::random()
    }

    #[test]
    fn a_request_with_no_listening_swap_is_left_unanswered() {
        assert_eq!(encsig_action(None, peer(), false), EncSigAction::LeaveUnanswered);
    }

    #[test]
    fn a_resend_for_an_already_stored_signature_is_acknowledged_again() {
        assert_eq!(encsig_action(None, peer(), true), EncSigAction::AckAlreadyProcessed);
    }

    #[test]
    fn a_request_from_the_swaps_taker_is_relayed() {
        let taker = peer();
        assert_eq!(encsig_action(Some(taker), taker, false), EncSigAction::Relay);
        assert_eq!(encsig_action(Some(taker), taker, true), EncSigAction::Relay);
    }

    // A third party who knew a swap id could consume the one-shot sender with garbage.
    #[test]
    fn a_request_from_another_peer_is_ignored_and_keeps_the_channel() {
        assert_eq!(encsig_action(Some(peer()), peer(), false), EncSigAction::WrongPeer);
        assert_eq!(encsig_action(Some(peer()), peer(), true), EncSigAction::WrongPeer);
    }
}

#[cfg(test)]
mod proof_latch_tests {
    use super::*;
    use crate::protocol_v5::core::monero::{PrivateKey, TransferProof, TxHash};

    fn handle() -> (BobEventLoopHandle, mpsc::UnboundedSender<transfer_proof::Request>) {
        let (new_swaps, _a) = bmrng::channel(1);
        let (commits, _b) = bmrng::channel(1);
        let (quote_requests, _c) = bmrng::channel(1);
        let (proof_tx, transfer_proofs) = mpsc::unbounded_channel();
        let (transfer_proof_acks, _d) = bmrng::channel(1);
        let (encrypted_signatures, _e) = bmrng::channel(1);
        // keep the receiving ends alive for the test's duration
        std::mem::forget((_a, _b, _c, _d, _e));
        (
            BobEventLoopHandle {
                new_swaps,
                commits,
                quote_requests,
                transfer_proofs,
                transfer_proof_acks,
                encrypted_signatures,
                latch: ProofLatch::default(),
            },
            proof_tx,
        )
    }

    fn request(swap_id: Uuid) -> transfer_proof::Request {
        transfer_proof::Request {
            swap_id,
            tx_lock_proof: TransferProof::new(
                TxHash("ab".repeat(32)),
                PrivateKey::from_slice(&[1u8; 32]).unwrap(),
            ),
        }
    }

    #[tokio::test]
    async fn a_second_wait_for_the_proof_returns_the_one_already_received() {
        let (mut handle, sender) = handle();
        let id = Uuid::new_v4();
        sender.send(request(id)).unwrap();

        let first = handle.recv_transfer_proof().await.unwrap();
        // Alice never resends: the channel is empty now.
        let second = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            handle.recv_transfer_proof(),
        )
        .await
        .expect("must not wait for a proof that was already received")
        .unwrap();

        assert_eq!(first.swap_id, second.swap_id);
        assert_eq!(first.tx_lock_proof, second.tx_lock_proof);
    }

    #[test]
    fn no_lock_is_recorded_as_accepted_until_the_flow_says_so() {
        let (handle, _sender) = handle();
        assert!(handle.accepted_state4().is_none());
    }
}

#[cfg(test)]
mod buffered_proof_tests {
    use super::*;

    type Responder = tokio::sync::oneshot::Sender<Result<()>>;

    #[test]
    fn buffered_entries_whose_swap_stopped_waiting_are_dropped() {
        let (waiting, _kept_alive) = tokio::sync::oneshot::channel::<Result<()>>();
        let (abandoned, receiver) = tokio::sync::oneshot::channel::<Result<()>>();
        drop(receiver); // the swap step timed out and was retried

        let mut buffer: Vec<Responder> = vec![abandoned, waiting];
        retain_waiting(&mut buffer, |responder| !responder.is_closed());

        assert_eq!(buffer.len(), 1);
        assert!(!buffer[0].is_closed());
    }

    #[test]
    fn repeated_retries_do_not_grow_the_buffer() {
        let mut buffer: Vec<Responder> = Vec::new();
        for _ in 0..50 {
            retain_waiting(&mut buffer, |responder| !responder.is_closed());
            let (responder, receiver) = tokio::sync::oneshot::channel::<Result<()>>();
            drop(receiver); // the attempt is abandoned right after it was buffered
            buffer.push(responder);
        }
        retain_waiting(&mut buffer, |responder| !responder.is_closed());
        assert!(buffer.is_empty());
    }
}

/// The Eigenwallet release whose protocol this ASB speaks. Eigenwallet's GUI reads the version from
/// the agent string and greys out makers below its minimum (2026-11-13: ours advertised the engine
/// crate's own 0.12.3 and showed "Maker version incompatible (too old)"). Keep this equal to the
/// release the swap-setup, timelock and fee code is checked against (docs/ATOMIC_SWAPS.md).
pub const ADVERTISED_ASB_VERSION: &str = "5.0.2";

/// The identify agent string peers see, for example `asb/5.0.2 (testnet)`.
pub fn asb_agent_version(namespace: &str) -> String {
    format!("asb/{ADVERTISED_ASB_VERSION} ({namespace})")
}

#[cfg(test)]
mod agent_version_tests {
    use super::*;

    #[test]
    fn agent_string_carries_the_eigenwallet_release_not_the_crate_version() {
        assert_eq!(asb_agent_version("testnet"), "asb/5.0.2 (testnet)");
        assert_ne!(ADVERTISED_ASB_VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn advertised_version_clears_eigenwallets_maker_floor() {
        // Eigenwallet's GUI floor is 4.13.0 in the source we compared against
        let mut parts = ADVERTISED_ASB_VERSION.split('.').map(|p| p.parse::<u32>().unwrap());
        let (major, minor) = (parts.next().unwrap(), parts.next().unwrap());
        assert!((major, minor) >= (4, 13));
    }
}
