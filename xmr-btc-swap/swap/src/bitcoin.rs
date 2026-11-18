pub mod wallet;

mod cancel;
mod lock;
mod punish;
mod redeem;
mod refund;
mod timelocks;

pub use crate::bitcoin::cancel::{CancelTimelock, PunishTimelock, TxCancel};
pub use crate::bitcoin::lock::TxLock;
pub use crate::bitcoin::punish::TxPunish;
pub use crate::bitcoin::redeem::TxRedeem;
pub use crate::bitcoin::refund::TxRefund;
pub use crate::bitcoin::timelocks::{BlockHeight, ExpiredTimelocks};
pub type Sighash = [u8; 32];

pub use ::bitcoin::Amount;
pub use ::bitcoin::psbt::Psbt as PartiallySignedTransaction;
pub use ::bitcoin::address::NetworkUnchecked;
pub use ::bitcoin::{Address, Network, Transaction, Txid};
pub use ecdsa_fun::adaptor::EncryptedSignature;
pub use ecdsa_fun::fun::Scalar;
pub use ecdsa_fun::Signature;
pub use wallet::Wallet;

/// Parse a user/peer supplied address into a network-checked address.
pub fn parse_checked_address(address: &str) -> Result<Address> {
    Ok(address
        .parse::<Address<NetworkUnchecked>>()
        .context("Invalid Bitcoin address")?
        .assume_checked())
}

pub mod address_serde {
    use super::Address;
    use ::bitcoin::address::NetworkUnchecked;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(address: &Address, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&address.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Address, D::Error>
    where
        D: Deserializer<'de>,
    {
        let address = Address::<NetworkUnchecked>::deserialize(deserializer)?;
        Ok(address.assume_checked())
    }
}

pub mod address_serde_opt {
    use super::Address;
    use ::bitcoin::address::NetworkUnchecked;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(address: &Option<Address>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match address {
            Some(address) => serializer.serialize_some(&address.to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Address>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let address = Option::<Address<NetworkUnchecked>>::deserialize(deserializer)?;
        Ok(address.map(|address| address.assume_checked()))
    }
}

pub fn to_ecdsa_signature(sig: Signature) -> ::bitcoin::ecdsa::Signature {
    ::bitcoin::ecdsa::Signature {
        signature: secp256k1::ecdsa::Signature::from_compact(&sig.to_bytes())
            .expect("valid compact signature"),
        sighash_type: ::bitcoin::EcdsaSighashType::All,
    }
}

#[cfg(test)]
pub use wallet::WalletBuilder;

use crate::bitcoin::wallet::ScriptStatus;
use ::bitcoin::hashes::Hash;
use ::bitcoin::secp256k1;
use anyhow::{bail, Context, Result};
use miniscript::descriptor::Wsh;
use miniscript::{Descriptor, Segwitv0};
use ecdsa_fun::adaptor::{Adaptor, HashTranscript};
use ecdsa_fun::fun::Point;
use ecdsa_fun::nonce::Deterministic;
use ecdsa_fun::ECDSA;
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::str::FromStr;

/// Bound a peer-proposed Bitcoin transaction fee (`tx_cancel_fee`, `tx_refund_fee`,
/// `tx_redeem_fee`, `tx_punish_fee` from the counterparty's swap-setup message) against our
/// own conservative fee estimate for the same transaction.
///
/// These fees are attacker-controlled: without this check a malicious counterparty can set
/// `tx_cancel_fee` close to the locked amount, so `lock_amount - fee` underflows when the
/// cancel/refund/punish transaction is built, producing a transaction that can never be
/// published - stranding the other party's already-locked funds with no way to cancel,
/// refund, or punish. Bounds mirror eigenwallet's `sanity_check_transaction_fee` fix
/// (4.6.6): reject anything below 50% or above 300% of our own estimate.
const MAX_FEE_OVERPAY_FACTOR: u64 = 3;
const MAX_FEE_UNDERPAY_FACTOR: u64 = 2;
const MIN_TRANSACTION_OUTPUT_SATS: u64 = 546;

/// Confirmation-urgency policy for the swap transactions, expressed as the
/// block target handed to the fee estimator (the Dojo estimator maps these
/// to confidence targets: 1→0.999, 2-3→0.99, 4-8→0.5, 9+→0.2).
///
/// Stakes, not just latency, drive the targets:
/// - [`FEE_TARGET_PRINCIPAL_CRITICAL`] (1): Alice's cancel/refund sanity
///   snapshots and the CLI Bob proposal. If either failure-path transaction
///   is stuck, the remote seller can punish — the locked BTC is LOST.
/// - [`FEE_TARGET_TIME_CRITICAL`] (2): the lock, the redeem, and the GUI
///   Bob cancel/refund proposal (which applies the 2× buffer). A stuck lock
///   just delays the swap; a stuck redeem fails the trade but no principal
///   is lost (Alice reclaims XMR via the Monero timelock, Bob refunds).
/// - [`FEE_TARGET_TIMELIGHT_BOUND`] (6): Alice's punish snapshot. It is only
///   a sanity floor for Bob's proposal; keeping this bar laxer avoids
///   rejecting swaps over rate disagreements.
pub mod fee_targets {
    pub const FEE_TARGET_PRINCIPAL_CRITICAL: usize = 1;
    pub const FEE_TARGET_TIME_CRITICAL: usize = 2;
    pub const FEE_TARGET_TIMELIGHT_BOUND: usize = 6;
}

/// Cancel and refund fees are signed into the swap at setup but may only be
/// broadcast hours later, if the swap fails. Their fee rate is doubled so an
/// ordinary fee spike after setup cannot strand the refund. Neither
/// transaction is published when the swap succeeds, so the buffer costs
/// nothing on the happy path.
pub const CANCEL_REFUND_FEE_RATE_MULTIPLIER: f32 = 2.0;

/// Lock and redeem are happy-path transactions, but their fee is fixed before
/// broadcast. Redeem in particular is broadcast after lock finality and the
/// Monero lock, so a small buffer against setup-to-broadcast drift is paid on
/// every successful swap. Keep this deliberately lower than the failure-path
/// cancel/refund buffer.
pub const LOCK_REDEEM_FEE_RATE_MULTIPLIER: f32 = 1.25;

/// Punish is a failure-path recovery transaction that can be broadcast days
/// after setup. The 2x buffer stays inside the 50%-300% peer sanity band.
pub const PUNISH_FEE_RATE_MULTIPLIER: f32 = 2.0;

pub fn sanity_check_transaction_fee(
    proposed_fee: Amount,
    own_conservative_estimate: Amount,
) -> Result<()> {
    let proposed_sat = proposed_fee.to_sat();
    let estimate_sat = own_conservative_estimate.to_sat();

    let ceiling_sat = estimate_sat
        .checked_mul(MAX_FEE_OVERPAY_FACTOR)
        .context("Fee ceiling overflowed while sanity-checking a peer-proposed fee")?;

    if proposed_sat > ceiling_sat {
        bail!(
            "Proposed fee {} is more than {}x our own conservative estimate of {} - refusing to accept, this may be an attempt to grief the swap",
            proposed_fee,
            MAX_FEE_OVERPAY_FACTOR,
            own_conservative_estimate
        );
    }

    let floor_sat = estimate_sat / MAX_FEE_UNDERPAY_FACTOR;

    if proposed_sat < floor_sat {
        bail!(
            "Proposed fee {} is less than {}x our own conservative estimate of {} - refusing to accept, this may be an attempt to grief the swap",
            proposed_fee,
            MAX_FEE_UNDERPAY_FACTOR,
            own_conservative_estimate
        );
    }

    Ok(())
}

/// Ensure that paying `fee` from `output` leaves a non-dust value. This is a
/// defensive bound in addition to [`sanity_check_transaction_fee`], because
/// the peer-supplied fee controls an unchecked `checked_sub` in transaction
/// construction.
pub fn sanity_check_fee_fits_output(fee: Amount, output: Amount) -> Result<()> {
    let remaining = output.to_sat().checked_sub(fee.to_sat()).context(
        "Peer-proposed fee exceeds the transaction output, transaction construction would underflow",
    )?;

    if remaining < MIN_TRANSACTION_OUTPUT_SATS {
        bail!(
            "Peer-proposed fee {} leaves only {} sats of a {} output, below the dust threshold",
            fee,
            remaining,
            output
        );
    }

    Ok(())
}

/// Bound the cancel and refund fees together: the refund spends the cancel
/// output, so checking each fee independently is not enough to prevent an
/// underflow when they are combined.
pub fn sanity_check_combined_fees(
    tx_cancel_fee: Amount,
    tx_refund_fee: Amount,
    lock_amount: Amount,
) -> Result<()> {
    let combined = tx_cancel_fee
        .to_sat()
        .checked_add(tx_refund_fee.to_sat())
        .context("Combined cancel and refund fees overflow")?;

    sanity_check_fee_fits_output(Amount::from_sat(combined), lock_amount)
        .context("Combined cancel and refund fees do not fit the lock output")
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Network")]
#[allow(non_camel_case_types)]
pub enum network {
    #[serde(rename = "Mainnet")]
    Bitcoin,
    Testnet,
    Testnet4,
    Signet,
    Regtest,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SecretKey {
    inner: Scalar,
    public: Point,
}

impl SecretKey {
    pub fn new_random<R: RngCore + CryptoRng>(rng: &mut R) -> Self {
        let scalar = Scalar::random(rng);

        let ecdsa = ECDSA::<()>::default();
        let public = ecdsa.verification_key_for(&scalar);

        Self {
            inner: scalar,
            public,
        }
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.public)
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.inner.to_bytes()
    }

    pub fn sign(&self, digest: Sighash) -> Signature {
        let ecdsa = ECDSA::<Deterministic<Sha256>>::default();

        ecdsa.sign(&self.inner, &digest)
    }

    // TxRefund encsigning explanation:
    //
    // A and B, are the Bitcoin Public Keys which go on the joint output for
    // TxLock_Bitcoin. S_a and S_b, are the Monero Public Keys which go on the
    // joint output for TxLock_Monero

    // tx_refund: multisig(A, B), published by bob
    // bob can produce sig on B using b
    // alice sends over an encrypted signature on A encrypted with S_b
    // s_b is leaked to alice when bob publishes signed tx_refund allowing her to
    // recover s_b: recover(encsig, S_b, sig_tx_refund) = s_b
    // alice now has s_a and s_b and can refund monero

    // self = a, Y = S_b, digest = tx_refund
    pub fn encsign(&self, Y: PublicKey, digest: Sighash) -> EncryptedSignature {
        let adaptor = Adaptor::<
            HashTranscript<Sha256, rand_chacha::ChaCha20Rng>,
            Deterministic<Sha256>,
        >::default();

        adaptor.encrypted_sign(&self.inner, &Y.0, &digest)
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicKey(Point);

impl PublicKey {
    #[cfg(test)]
    pub fn random() -> Self {
        Self(Point::random(&mut rand::thread_rng()))
    }
}

impl From<PublicKey> for Point {
    fn from(from: PublicKey) -> Self {
        from.0
    }
}

impl TryFrom<PublicKey> for bitcoin::PublicKey {
    type Error = bitcoin::key::FromSliceError;

    fn try_from(pubkey: PublicKey) -> Result<Self, Self::Error> {
        let bytes = pubkey.0.to_bytes();
        bitcoin::PublicKey::from_slice(&bytes)
    }
}

impl From<Point> for PublicKey {
    fn from(p: Point) -> Self {
        Self(p)
    }
}

impl From<Scalar> for SecretKey {
    fn from(scalar: Scalar) -> Self {
        let ecdsa = ECDSA::<()>::default();
        let public = ecdsa.verification_key_for(&scalar);

        Self {
            inner: scalar,
            public,
        }
    }
}

impl From<SecretKey> for Scalar {
    fn from(sk: SecretKey) -> Self {
        sk.inner
    }
}

impl From<Scalar> for PublicKey {
    fn from(scalar: Scalar) -> Self {
        let ecdsa = ECDSA::<()>::default();
        PublicKey(ecdsa.verification_key_for(&scalar))
    }
}

pub fn verify_sig(
    verification_key: &PublicKey,
    transaction_sighash: &Sighash,
    sig: &Signature,
) -> Result<()> {
    let ecdsa = ECDSA::verify_only();

    if ecdsa.verify(&verification_key.0, transaction_sighash, sig) {
        Ok(())
    } else {
        bail!(InvalidSignature)
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("signature is invalid")]
pub struct InvalidSignature;

pub fn verify_encsig(
    verification_key: PublicKey,
    encryption_key: PublicKey,
    digest: &Sighash,
    encsig: &EncryptedSignature,
) -> Result<()> {
    let adaptor = Adaptor::<HashTranscript<Sha256>, Deterministic<Sha256>>::default();

    if adaptor.verify_encrypted_signature(
        &verification_key.0,
        &encryption_key.0,
        digest,
        encsig,
    ) {
        Ok(())
    } else {
        bail!(InvalidEncryptedSignature)
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("encrypted signature is invalid")]
pub struct InvalidEncryptedSignature;

pub fn build_shared_output_descriptor(A: Point, B: Point) -> Descriptor<bitcoin::PublicKey> {
    const MINISCRIPT_TEMPLATE: &str = "c:and_v(v:pk(A),pk_k(B))";

    // NOTE: This shouldn't be a source of error, but maybe it is
    let A = hex::encode(secp256k1::PublicKey::from_slice(&A.to_bytes()).expect("valid public key").serialize());
    let B = hex::encode(secp256k1::PublicKey::from_slice(&B.to_bytes()).expect("valid public key").serialize());

    let miniscript = MINISCRIPT_TEMPLATE.replace('A', &A).replace('B', &B);

    let miniscript =
        miniscript::Miniscript::<bitcoin::PublicKey, Segwitv0>::from_str(&miniscript)
            .expect("a valid miniscript");

    Descriptor::Wsh(Wsh::new(miniscript).expect("a valid descriptor"))
}

pub fn recover(S: PublicKey, sig: Signature, encsig: EncryptedSignature) -> Result<SecretKey> {
    let adaptor = Adaptor::<HashTranscript<Sha256>, Deterministic<Sha256>>::default();

    let s = adaptor
        .recover_decryption_key(&S.0, &sig, &encsig)
        .map(SecretKey::from)
        .context("Failed to recover secret from adaptor signature")?;

    Ok(s)
}

pub fn current_epoch(
    cancel_timelock: CancelTimelock,
    punish_timelock: PunishTimelock,
    tx_lock_status: ScriptStatus,
    tx_cancel_status: ScriptStatus,
) -> ExpiredTimelocks {
    if tx_cancel_status.is_confirmed_with(punish_timelock) {
        return ExpiredTimelocks::Punish;
    }

    if tx_lock_status.is_confirmed_with(cancel_timelock) {
        return ExpiredTimelocks::Cancel;
    }

    ExpiredTimelocks::None
}

/// Bitcoin error codes: https://github.com/bitcoin/bitcoin/blob/97d3500601c1d28642347d014a6de1e38f53ae4e/src/rpc/protocol.h#L23
pub enum RpcErrorCode {
    /// Transaction or block was rejected by network rules. Error code -26.
    RpcVerifyRejected,
    /// Transaction or block was rejected by network rules. Error code -27.
    RpcVerifyAlreadyInChain,
    /// General error during transaction or block submission
    RpcVerifyError,
}

impl From<RpcErrorCode> for i64 {
    fn from(code: RpcErrorCode) -> Self {
        match code {
            RpcErrorCode::RpcVerifyError => -25,
            RpcErrorCode::RpcVerifyRejected => -26,
            RpcErrorCode::RpcVerifyAlreadyInChain => -27,
        }
    }
}

pub fn parse_rpc_error_code(error: &anyhow::Error) -> anyhow::Result<i64> {
    let string = match error.downcast_ref::<bdk_electrum::electrum_client::Error>() {
        Some(bdk_electrum::electrum_client::Error::Protocol(
            serde_json::Value::String(string),
        )) => string,
        _ => bail!("Error is of incorrect variant:{}", error),
    };

    let json = serde_json::from_str(&string.replace("sendrawtransaction RPC error:", ""))?;

    let json_map = match json {
        serde_json::Value::Object(map) => map,
        _ => bail!("Json error is not json object "),
    };

    let error_code_value = match json_map.get("code") {
        Some(val) => val,
        None => bail!("No error code field"),
    };

    let error_code_number = match error_code_value {
        serde_json::Value::Number(num) => num,
        _ => bail!("Error code is not a number"),
    };

    if let Some(int) = error_code_number.as_i64() {
        Ok(int)
    } else {
        bail!("Error code is not an unsigned integer")
    }
}

#[derive(Clone, Copy, thiserror::Error, Debug)]
#[error("transaction does not spend anything")]
pub struct NoInputs;

#[derive(Clone, Copy, thiserror::Error, Debug)]
#[error("transaction has {0} inputs, expected 1")]
pub struct TooManyInputs(usize);

#[derive(Clone, Copy, thiserror::Error, Debug)]
#[error("empty witness stack")]
pub struct EmptyWitnessStack;

#[derive(Clone, Copy, thiserror::Error, Debug)]
#[error("input has {0} witnesses, expected 3")]
pub struct NotThreeWitnesses(usize);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{GetConfig, Regtest};
    use crate::protocol::bob::BobState;
    use crate::protocol::{alice, bob};
    use rand::rngs::OsRng;
    use uuid::Uuid;

    #[test]
    fn lock_confirmations_le_to_cancel_timelock_no_timelock_expired() {
        let tx_lock_status = ScriptStatus::from_confirmations(4);
        let tx_cancel_status = ScriptStatus::Unseen;

        let expired_timelock = current_epoch(
            CancelTimelock::new(5),
            PunishTimelock::new(5),
            tx_lock_status,
            tx_cancel_status,
        );

        assert_eq!(expired_timelock, ExpiredTimelocks::None)
    }

    #[test]
    fn lock_confirmations_ge_to_cancel_timelock_cancel_timelock_expired() {
        let tx_lock_status = ScriptStatus::from_confirmations(5);
        let tx_cancel_status = ScriptStatus::Unseen;

        let expired_timelock = current_epoch(
            CancelTimelock::new(5),
            PunishTimelock::new(5),
            tx_lock_status,
            tx_cancel_status,
        );

        assert_eq!(expired_timelock, ExpiredTimelocks::Cancel)
    }

    #[test]
    fn cancel_confirmations_ge_to_punish_timelock_punish_timelock_expired() {
        let tx_lock_status = ScriptStatus::from_confirmations(10);
        let tx_cancel_status = ScriptStatus::from_confirmations(5);

        let expired_timelock = current_epoch(
            CancelTimelock::new(5),
            PunishTimelock::new(5),
            tx_lock_status,
            tx_cancel_status,
        );

        assert_eq!(expired_timelock, ExpiredTimelocks::Punish)
    }

    #[tokio::test]
    async fn spending_transaction_construction_rejects_fee_at_or_above_output() {
        let wallet = WalletBuilder::new(Amount::ONE_BTC.to_sat()).build();
        let amount = Amount::from_sat(100_000);
        let change = wallet.new_address().await.unwrap();
        let tx_lock = TxLock::new(
            &wallet,
            amount,
            PublicKey::random(),
            PublicKey::random(),
            change.clone(),
        )
        .await
        .unwrap();

        let err = tx_lock
            .build_spend_transaction(&change, None, tx_lock.lock_amount())
            .unwrap_err();
        assert!(err.to_string().contains("must be less than lock output value"));

        let tx_cancel = TxCancel::new(
            &tx_lock,
            CancelTimelock::new(1),
            PublicKey::random(),
            PublicKey::random(),
            Amount::from_sat(100),
        );
        let err = tx_cancel
            .build_spend_transaction(&change, None, tx_cancel.amount())
            .unwrap_err();
        assert!(err.to_string().contains("must be less than cancel output value"));
    }

    #[tokio::test]
    async fn lock_recovery_state_round_trips_and_transaction_weights_remain_valid() {
        let alice_wallet = WalletBuilder::new(Amount::ONE_BTC.to_sat()).build();
        let bob_wallet = WalletBuilder::new(Amount::ONE_BTC.to_sat()).build();
        let btc_amount = Amount::from_sat(500_000);
        let xmr_amount = crate::monero::Amount::from_piconero(10000);

        let tx_redeem_fee = alice_wallet
            .estimate_fee_vsize(TxRedeem::vsize(), btc_amount)
            .await
            .unwrap();
        let tx_punish_fee = alice_wallet
            .estimate_fee_vsize(TxPunish::vsize(), btc_amount)
            .await
            .unwrap();
        let tx_cancel_fee = alice_wallet
            .estimate_fee_vsize(TxCancel::vsize(), btc_amount)
            .await
            .unwrap();
        let tx_refund_fee = alice_wallet
            .estimate_fee_vsize(TxRefund::vsize(), btc_amount)
            .await
            .unwrap();
        let redeem_address = alice_wallet.new_address().await.unwrap();
        let punish_address = alice_wallet.new_address().await.unwrap();

        let config = Regtest::get_config();
        let alice_state0 = alice::State0::new(
            btc_amount,
            xmr_amount,
            config,
            redeem_address,
            punish_address,
            tx_redeem_fee,
            tx_punish_fee,
            tx_cancel_fee,
            tx_refund_fee,
            &mut OsRng,
        );

        let bob_state0 = bob::State0::new(
            Uuid::new_v4(),
            &mut OsRng,
            btc_amount,
            xmr_amount,
            config.bitcoin_cancel_timelock,
            config.bitcoin_punish_timelock,
            bob_wallet.new_address().await.unwrap(),
            config.monero_finality_confirmations,
            tx_refund_fee,
            tx_cancel_fee,
        );

        let message0 = bob_state0.next_message();

        let (_, alice_state1) = alice_state0.receive(message0).unwrap();
        let alice_message1 = alice_state1.next_message();

        let bob_state1 = bob_state0
            .receive(&bob_wallet, alice_message1)
            .await
            .unwrap();
        let bob_message2 = bob_state1.next_message();

        let alice_state2 = alice_state1.receive(bob_message2).unwrap();
        let alice_message3 = alice_state2.next_message().unwrap();

        let bob_state2 = bob_state1.receive(alice_message3).unwrap();
        let bob_message4 = bob_state2.next_message().unwrap();

        let alice_state3 = alice_state2.receive(bob_message4).unwrap();

        let (bob_state3, tx_lock) = bob_state2.lock_btc().await.unwrap();
        let signed_tx = bob_wallet.sign_and_finalize(tx_lock.into()).await.unwrap();
        let signed_lock_state = BobState::BtcLockSigned {
            state3: bob_state3.clone(),
            signed_transaction: signed_tx,
            monero_wallet_restore_blockheight: monero_rpc::wallet::BlockHeight { height: 42 },
        };
        assert!(bob_state3.cancel().signed_refund_transaction().is_ok());

        // A restart must restore the same signed lock transaction and protocol
        // state, rather than reconstructing a potentially different transaction.
        let persisted = crate::database::Bob::from(signed_lock_state.clone());
        let serialized = serde_json::to_vec(&persisted).unwrap();
        let restored: crate::database::Bob = serde_json::from_slice(&serialized).unwrap();
        let restored: BobState = restored.into();
        assert_eq!(restored, signed_lock_state);

        let bob_state4 = bob_state3.xmr_locked(monero_rpc::wallet::BlockHeight { height: 0 });
        let encrypted_signature = bob_state4.tx_redeem_encsig().unwrap();
        let bob_state6 = bob_state4.cancel();

        let cancel_transaction = alice_state3.signed_cancel_transaction().unwrap();
        let punish_transaction = alice_state3.signed_punish_transaction().unwrap();
        let redeem_transaction = alice_state3
            .signed_redeem_transaction(encrypted_signature)
            .unwrap();
        let refund_transaction = bob_state6.signed_refund_transaction().unwrap();

        assert_weight(redeem_transaction, TxRedeem::weight(), "TxRedeem");
        assert_weight(cancel_transaction, TxCancel::weight(), "TxCancel");
        assert_weight(punish_transaction, TxPunish::weight(), "TxPunish");
        assert_weight(refund_transaction, TxRefund::weight(), "TxRefund");
    }

    // Weights fluctuate because of the length of the signatures. Valid ecdsa
    // signatures can have 68, 69, 70, 71, or 72 bytes. Since most of our
    // transactions have 2 signatures the weight can be up to 8 bytes less than
    // the static weight (4 bytes per signature).
    fn assert_weight(transaction: Transaction, expected_weight: usize, tx_name: &str) {
        let is_weight = transaction.weight();

        assert!(
            expected_weight.saturating_sub(is_weight.to_wu() as usize) <= 8,
            "{} to have weight {}, but was {}. Transaction: {:#?}",
            tx_name,
            expected_weight,
            is_weight,
            transaction
        )
    }

    #[test]
    fn buffered_fee_proposals_stay_inside_the_peer_sanity_band() {
        let estimate = Amount::from_sat(1_000);

        for multiplier in [
            LOCK_REDEEM_FEE_RATE_MULTIPLIER,
            CANCEL_REFUND_FEE_RATE_MULTIPLIER,
            PUNISH_FEE_RATE_MULTIPLIER,
        ] {
            assert!(f64::from(multiplier) < MAX_FEE_OVERPAY_FACTOR as f64);
            let proposed = Amount::from_sat((1_000.0 * f64::from(multiplier)) as u64);
            sanity_check_transaction_fee(proposed, estimate)
                .expect("our own setup-time buffers must pass the peer sanity band");
        }
    }
}
