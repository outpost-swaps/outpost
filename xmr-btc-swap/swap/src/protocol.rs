use crate::protocol::alice::swap::is_complete as alice_is_complete;
use crate::protocol::alice::AliceState;
use crate::protocol::bob::swap::is_complete as bob_is_complete;
use crate::protocol::bob::BobState;
use crate::{bitcoin, monero};
use anyhow::Result;
use async_trait::async_trait;
use conquer_once::Lazy;
use libp2p::{Multiaddr, PeerId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigma_fun::ext::dl_secp256k1_ed25519_eq::{CrossCurveDLEQ, CrossCurveDLEQProof};
use sigma_fun::HashTranscript;
use std::convert::TryInto;
use uuid::Uuid;

pub mod alice;
pub mod bob;

/// BIP-341 NUMS point `H = lift_x(SHA256(uncompressed_encoding(G)))`: <https://github.com/bitcoin/bips/blob/master/bip-0341.mediawiki#constructing-and-spending-taproot-outputs>
///
/// The cross-curve DLEQ proof system must be built from a generator with an unknown
/// discrete logarithm relationship to the basepoint; using the standard basepoints
/// (as older forks of this crate did) weakens the proof's soundness. Must stay in
/// sync with the same fix in the eigenwallet core fork (commit `1d3ee9da6`) so that
/// proofs verify across both networks.
static NUMS_H_SECP256K1: Lazy<ecdsa_fun::fun::Point> = Lazy::new(|| {
    let generator = (*ecdsa_fun::fun::G).normalize().to_bytes_uncompressed();
    let x_coordinate = Sha256::digest(generator).into();

    ecdsa_fun::fun::Point::<ecdsa_fun::fun::marker::EvenY>::from_xonly_bytes(x_coordinate)
        .expect("SHA-256 of the uncompressed secp256k1 generator is a valid x-coordinate")
        .normalize()
});

/// Monero's Pedersen commitment generator `H` (`rctTypes.h`), the canonical
/// unknown-discrete-log point for ed25519 in this proof system.
static PEDERSEN_BLINDING_H_ED25519: Lazy<curve25519_dalek::edwards::EdwardsPoint> =
    Lazy::new(|| {
        curve25519_dalek::edwards::CompressedEdwardsY(::monero::util::key::H.point.0)
            .decompress()
            .expect("Monero Pedersen H is a valid ed25519 point")
    });

pub static CROSS_CURVE_PROOF_SYSTEM: Lazy<
    CrossCurveDLEQ<HashTranscript<Sha256, rand_chacha::ChaCha20Rng>>,
> = Lazy::new(|| {
    CrossCurveDLEQ::<HashTranscript<Sha256, rand_chacha::ChaCha20Rng>>::new(
        *NUMS_H_SECP256K1,
        *PEDERSEN_BLINDING_H_ED25519,
    )
});

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message0 {
    swap_id: Uuid,
    B: bitcoin::PublicKey,
    S_b_monero: monero::PublicKey,
    S_b_bitcoin: bitcoin::PublicKey,
    dleq_proof_s_b: CrossCurveDLEQProof,
    v_b: monero::PrivateViewKey,
    #[serde(with = "crate::bitcoin::address_serde")]
    refund_address: bitcoin::Address,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_refund_fee: bitcoin::Amount,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_cancel_fee: bitcoin::Amount,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message1 {
    A: bitcoin::PublicKey,
    S_a_monero: monero::PublicKey,
    S_a_bitcoin: bitcoin::PublicKey,
    dleq_proof_s_a: CrossCurveDLEQProof,
    v_a: monero::PrivateViewKey,
    #[serde(with = "crate::bitcoin::address_serde")]
    redeem_address: bitcoin::Address,
    #[serde(with = "crate::bitcoin::address_serde")]
    punish_address: bitcoin::Address,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_redeem_fee: bitcoin::Amount,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_punish_fee: bitcoin::Amount,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message2 {
    psbt: bitcoin::PartiallySignedTransaction,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message3 {
    tx_cancel_sig: bitcoin::Signature,
    tx_refund_encsig: bitcoin::EncryptedSignature,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message4 {
    tx_punish_sig: bitcoin::Signature,
    tx_cancel_sig: bitcoin::Signature,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Alice(AliceState),
    Bob(BobState),
}

impl State {
    pub fn swap_finished(&self) -> bool {
        match self {
            State::Alice(state) => alice_is_complete(state),
            State::Bob(state) => bob_is_complete(state),
        }
    }
}

impl From<AliceState> for State {
    fn from(alice: AliceState) -> Self {
        Self::Alice(alice)
    }
}

impl From<BobState> for State {
    fn from(bob: BobState) -> Self {
        Self::Bob(bob)
    }
}

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("Not in the role of Alice")]
pub struct NotAlice;

#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("Not in the role of Bob")]
pub struct NotBob;

impl TryInto<BobState> for State {
    type Error = NotBob;

    fn try_into(self) -> std::result::Result<BobState, Self::Error> {
        match self {
            State::Alice(_) => Err(NotBob),
            State::Bob(state) => Ok(state),
        }
    }
}

impl TryInto<AliceState> for State {
    type Error = NotAlice;

    fn try_into(self) -> std::result::Result<AliceState, Self::Error> {
        match self {
            State::Alice(state) => Ok(state),
            State::Bob(_) => Err(NotAlice),
        }
    }
}

#[async_trait]
pub trait Database: Send + Sync {
    async fn insert_peer_id(&self, swap_id: Uuid, peer_id: PeerId) -> Result<()>;
    async fn get_peer_id(&self, swap_id: Uuid) -> Result<PeerId>;
    async fn insert_monero_address(&self, swap_id: Uuid, address: monero::Address) -> Result<()>;
    async fn get_monero_address(&self, swap_id: Uuid) -> Result<monero::Address>;
    async fn insert_address(&self, peer_id: PeerId, address: Multiaddr) -> Result<()>;
    async fn get_addresses(&self, peer_id: PeerId) -> Result<Vec<Multiaddr>>;
    async fn insert_latest_state(&self, swap_id: Uuid, state: State) -> Result<()>;
    async fn insert_redeem_sweep_txid(&self, swap_id: Uuid, txid: crate::monero::TxHash)
        -> Result<()>;
    async fn get_redeem_sweep_txid(&self, swap_id: Uuid) -> Result<Option<crate::monero::TxHash>>;
    async fn get_state(&self, swap_id: Uuid) -> Result<State>;
    /// Returns every persisted state row for the given swap, ordered oldest to
    /// newest — unlike `get_state`/`all`, which only surface the latest row.
    async fn get_all_states(&self, swap_id: Uuid) -> Result<Vec<State>>;
    async fn all(&self) -> Result<Vec<(Uuid, State)>>;
}
