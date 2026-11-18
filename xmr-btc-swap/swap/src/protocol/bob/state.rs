use crate::bitcoin::wallet::{EstimateFeeRate, Subscription};
use crate::bitcoin::{
    self, current_epoch, CancelTimelock, ExpiredTimelocks, PunishTimelock, Transaction, TxCancel,
    TxLock, TxPunish, TxRedeem, Txid,
};
use crate::monero;
use crate::monero::wallet::WatchRequest;
use crate::monero::{monero_private_key, TransferProof};
use crate::monero_ext::ScalarExt;
use crate::protocol::{Message0, Message1, Message2, Message3, Message4, CROSS_CURVE_PROOF_SYSTEM};
use anyhow::{anyhow, bail, Context, Result};
use ecdsa_fun::adaptor::{Adaptor, HashTranscript};
use ecdsa_fun::nonce::Deterministic;
use ecdsa_fun::Signature;
use monero_rpc::wallet::BlockHeight;
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sigma_fun::ext::dl_secp256k1_ed25519_eq::CrossCurveDLEQProof;
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum BobState {
    Started {
        btc_amount: bitcoin::Amount,
        change_address: bitcoin::Address,
    },
    SwapSetupCompleted(State2),
    BtcLockSigned {
        state3: State3,
        signed_transaction: bitcoin::Transaction,
        monero_wallet_restore_blockheight: BlockHeight,
    },
    BtcLocked {
        state3: State3,
        monero_wallet_restore_blockheight: BlockHeight,
    },
    XmrLockProofReceived {
        state: State3,
        lock_transfer_proof: TransferProof,
        monero_wallet_restore_blockheight: BlockHeight,
    },
    XmrLocked(State4),
    EncSigSent(State4),
    BtcRedeemed(State5),
    CancelTimelockExpired(State6),
    BtcCancelled(State6),
    BtcRefunded(State6),
    XmrRedeemed {
        tx_lock_id: bitcoin::Txid,
    },
    BtcPunished {
        tx_lock_id: bitcoin::Txid,
    },
    SafelyAborted,
}

impl fmt::Display for BobState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BobState::Started { .. } => write!(f, "quote has been requested"),
            BobState::SwapSetupCompleted(..) => write!(f, "execution setup done"),
            BobState::BtcLockSigned { .. } => write!(f, "Bitcoin lock transaction signed"),
            BobState::BtcLocked { .. } => write!(f, "btc is locked"),
            BobState::XmrLockProofReceived { .. } => {
                write!(f, "XMR lock transaction transfer proof received")
            }
            BobState::XmrLocked(..) => write!(f, "xmr is locked"),
            BobState::EncSigSent(..) => write!(f, "encrypted signature is sent"),
            BobState::BtcRedeemed(..) => write!(f, "btc is redeemed"),
            BobState::CancelTimelockExpired(..) => write!(f, "cancel timelock is expired"),
            BobState::BtcCancelled(..) => write!(f, "btc is cancelled"),
            BobState::BtcRefunded(..) => write!(f, "btc is refunded"),
            BobState::XmrRedeemed { .. } => write!(f, "xmr is redeemed"),
            BobState::BtcPunished { .. } => write!(f, "btc is punished"),
            BobState::SafelyAborted => write!(f, "safely aborted"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct State0 {
    swap_id: Uuid,
    b: bitcoin::SecretKey,
    s_b: monero::Scalar,
    S_b_monero: monero::PublicKey,
    S_b_bitcoin: bitcoin::PublicKey,
    v_b: monero::PrivateViewKey,
    dleq_proof_s_b: CrossCurveDLEQProof,
    btc: bitcoin::Amount,
    xmr: monero::Amount,
    cancel_timelock: CancelTimelock,
    punish_timelock: PunishTimelock,
    refund_address: bitcoin::Address,
    min_monero_confirmations: u64,
    tx_refund_fee: bitcoin::Amount,
    tx_cancel_fee: bitcoin::Amount,
}

impl State0 {
    #[allow(clippy::too_many_arguments)]
    pub fn new<R: RngCore + CryptoRng>(
        swap_id: Uuid,
        rng: &mut R,
        btc: bitcoin::Amount,
        xmr: monero::Amount,
        cancel_timelock: CancelTimelock,
        punish_timelock: PunishTimelock,
        refund_address: bitcoin::Address,
        min_monero_confirmations: u64,
        tx_refund_fee: bitcoin::Amount,
        tx_cancel_fee: bitcoin::Amount,
    ) -> Self {
        let b = bitcoin::SecretKey::new_random(rng);

        let s_b = monero::Scalar::random(rng);
        let v_b = monero::PrivateViewKey::new_random(rng);

        let (dleq_proof_s_b, (S_b_bitcoin, S_b_monero)) = CROSS_CURVE_PROOF_SYSTEM.prove(&s_b, rng);

        Self {
            swap_id,
            b,
            s_b,
            v_b,
            S_b_bitcoin: bitcoin::PublicKey::from(S_b_bitcoin),
            S_b_monero: monero::PublicKey {
                point: S_b_monero.compress(),
            },
            btc,
            xmr,
            dleq_proof_s_b,
            cancel_timelock,
            punish_timelock,
            refund_address,
            min_monero_confirmations,
            tx_refund_fee,
            tx_cancel_fee,
        }
    }

    pub fn next_message(&self) -> Message0 {
        Message0 {
            swap_id: self.swap_id,
            B: self.b.public(),
            S_b_monero: self.S_b_monero,
            S_b_bitcoin: self.S_b_bitcoin,
            dleq_proof_s_b: self.dleq_proof_s_b.clone(),
            v_b: self.v_b,
            refund_address: self.refund_address.clone(),
            tx_refund_fee: self.tx_refund_fee,
            tx_cancel_fee: self.tx_cancel_fee,
        }
    }

    pub async fn receive<C>(
        self,
        wallet: &bitcoin::Wallet<C>,
        msg: Message1,
    ) -> Result<State1>
    where
        C: EstimateFeeRate + Send + Sync + 'static,
    {
        let valid = CROSS_CURVE_PROOF_SYSTEM.verify(
            &msg.dleq_proof_s_a,
            (
                msg.S_a_bitcoin.into(),
                msg.S_a_monero
                    .point
                    .decompress()
                    .ok_or_else(|| anyhow!("S_a is not a monero curve point"))?,
            ),
        );

        if !valid {
            bail!("Alice's dleq proof doesn't verify")
        }

        let own_tx_redeem_fee_estimate = wallet
            .estimate_fee_vsize_at(
                TxRedeem::vsize(),
                self.btc,
                bitcoin::fee_targets::FEE_TARGET_TIME_CRITICAL,
            )
            .await
            .context("Failed to estimate our own tx_redeem_fee")?;
        bitcoin::sanity_check_transaction_fee(msg.tx_redeem_fee, own_tx_redeem_fee_estimate)
            .context("Alice's proposed tx_redeem_fee failed sanity check")?;
        bitcoin::sanity_check_fee_fits_output(msg.tx_redeem_fee, self.btc)
            .context("Alice's proposed tx_redeem_fee does not fit the lock output")?;

        let own_tx_punish_fee_estimate = wallet
            .estimate_fee_vsize_at(
                TxPunish::vsize(),
                self.btc,
                bitcoin::fee_targets::FEE_TARGET_TIMELIGHT_BOUND,
            )
            .await
            .context("Failed to estimate our own tx_punish_fee")?;
        bitcoin::sanity_check_transaction_fee(msg.tx_punish_fee, own_tx_punish_fee_estimate)
            .context("Alice's proposed tx_punish_fee failed sanity check")?;
        let punish_output = self
            .btc
            .to_sat()
            .checked_sub(self.tx_cancel_fee.to_sat())
            .context("Bob's own tx_cancel_fee exceeds the lock amount")?;
        bitcoin::sanity_check_fee_fits_output(
            msg.tx_punish_fee,
            bitcoin::Amount::from_sat(punish_output),
        )
        .context("Alice's proposed tx_punish_fee does not fit the cancel output")?;

        let tx_lock = bitcoin::TxLock::new(
            wallet,
            self.btc,
            msg.A,
            self.b.public(),
            self.refund_address.clone(),
        )
        .await?;
        let v = msg.v_a + self.v_b;

        Ok(State1 {
            A: msg.A,
            b: self.b,
            s_b: self.s_b,
            S_a_monero: msg.S_a_monero,
            S_a_bitcoin: msg.S_a_bitcoin,
            v,
            xmr: self.xmr,
            cancel_timelock: self.cancel_timelock,
            punish_timelock: self.punish_timelock,
            refund_address: self.refund_address,
            redeem_address: msg.redeem_address,
            punish_address: msg.punish_address,
            tx_lock,
            min_monero_confirmations: self.min_monero_confirmations,
            tx_redeem_fee: msg.tx_redeem_fee,
            tx_refund_fee: self.tx_refund_fee,
            tx_punish_fee: msg.tx_punish_fee,
            tx_cancel_fee: self.tx_cancel_fee,
        })
    }
}

#[derive(Debug)]
pub struct State1 {
    A: bitcoin::PublicKey,
    b: bitcoin::SecretKey,
    s_b: monero::Scalar,
    S_a_monero: monero::PublicKey,
    S_a_bitcoin: bitcoin::PublicKey,
    v: monero::PrivateViewKey,
    xmr: monero::Amount,
    cancel_timelock: CancelTimelock,
    punish_timelock: PunishTimelock,
    refund_address: bitcoin::Address,
    redeem_address: bitcoin::Address,
    punish_address: bitcoin::Address,
    tx_lock: bitcoin::TxLock,
    min_monero_confirmations: u64,
    tx_redeem_fee: bitcoin::Amount,
    tx_refund_fee: bitcoin::Amount,
    tx_punish_fee: bitcoin::Amount,
    tx_cancel_fee: bitcoin::Amount,
}

impl State1 {
    pub fn next_message(&self) -> Message2 {
        Message2 {
            psbt: self.tx_lock.clone().into(),
        }
    }

    pub fn receive(self, msg: Message3) -> Result<State2> {
        let tx_cancel = TxCancel::new(
            &self.tx_lock,
            self.cancel_timelock,
            self.A,
            self.b.public(),
            self.tx_cancel_fee,
        );
        let tx_refund =
            bitcoin::TxRefund::new(&tx_cancel, &self.refund_address, self.tx_refund_fee)?;

        bitcoin::verify_sig(&self.A, &tx_cancel.digest(), &msg.tx_cancel_sig)?;
        bitcoin::verify_encsig(
            self.A,
            bitcoin::PublicKey::from(self.s_b.to_secpfun_scalar()),
            &tx_refund.digest(),
            &msg.tx_refund_encsig,
        )?;

        Ok(State2 {
            A: self.A,
            b: self.b,
            s_b: self.s_b,
            S_a_monero: self.S_a_monero,
            S_a_bitcoin: self.S_a_bitcoin,
            v: self.v,
            xmr: self.xmr,
            cancel_timelock: self.cancel_timelock,
            punish_timelock: self.punish_timelock,
            refund_address: self.refund_address,
            redeem_address: self.redeem_address,
            punish_address: self.punish_address,
            tx_lock: self.tx_lock,
            tx_cancel_sig_a: msg.tx_cancel_sig,
            tx_refund_encsig: msg.tx_refund_encsig,
            min_monero_confirmations: self.min_monero_confirmations,
            tx_redeem_fee: self.tx_redeem_fee,
            tx_refund_fee: self.tx_refund_fee,
            tx_punish_fee: self.tx_punish_fee,
            tx_cancel_fee: self.tx_cancel_fee,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct State2 {
    A: bitcoin::PublicKey,
    b: bitcoin::SecretKey,
    s_b: monero::Scalar,
    S_a_monero: monero::PublicKey,
    S_a_bitcoin: bitcoin::PublicKey,
    v: monero::PrivateViewKey,
    xmr: monero::Amount,
    cancel_timelock: CancelTimelock,
    punish_timelock: PunishTimelock,
    #[serde(with = "crate::bitcoin::address_serde")]
    refund_address: bitcoin::Address,
    #[serde(with = "crate::bitcoin::address_serde")]
    redeem_address: bitcoin::Address,
    #[serde(with = "crate::bitcoin::address_serde")]
    punish_address: bitcoin::Address,
    tx_lock: bitcoin::TxLock,
    tx_cancel_sig_a: Signature,
    tx_refund_encsig: bitcoin::EncryptedSignature,
    min_monero_confirmations: u64,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_redeem_fee: bitcoin::Amount,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_punish_fee: bitcoin::Amount,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_refund_fee: bitcoin::Amount,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_cancel_fee: bitcoin::Amount,
}

impl State2 {
    pub fn btc_amount(&self) -> bitcoin::Amount {
        self.tx_lock.lock_amount()
    }

    pub fn xmr_amount(&self) -> monero::Amount {
        self.xmr
    }

    pub fn next_message(&self) -> Result<Message4> {
        let tx_cancel = TxCancel::new(
            &self.tx_lock,
            self.cancel_timelock,
            self.A,
            self.b.public(),
            self.tx_cancel_fee,
        );
        let tx_cancel_sig = self.b.sign(tx_cancel.digest());
        let tx_punish = bitcoin::TxPunish::new(
            &tx_cancel,
            &self.punish_address,
            self.punish_timelock,
            self.tx_punish_fee,
        )?;
        let tx_punish_sig = self.b.sign(tx_punish.digest());

        Ok(Message4 {
            tx_punish_sig,
            tx_cancel_sig,
        })
    }

    pub async fn lock_btc(self) -> Result<(State3, TxLock)> {
        Ok((
            State3 {
                A: self.A,
                b: self.b,
                s_b: self.s_b,
                S_a_monero: self.S_a_monero,
                S_a_bitcoin: self.S_a_bitcoin,
                v: self.v,
                xmr: self.xmr,
                cancel_timelock: self.cancel_timelock,
                punish_timelock: self.punish_timelock,
                refund_address: self.refund_address,
                redeem_address: self.redeem_address,
                tx_lock: self.tx_lock.clone(),
                tx_cancel_sig_a: self.tx_cancel_sig_a,
                tx_refund_encsig: self.tx_refund_encsig,
                min_monero_confirmations: self.min_monero_confirmations,
                tx_redeem_fee: self.tx_redeem_fee,
                tx_refund_fee: self.tx_refund_fee,
                tx_cancel_fee: self.tx_cancel_fee,
            },
            self.tx_lock,
        ))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct State3 {
    A: bitcoin::PublicKey,
    b: bitcoin::SecretKey,
    s_b: monero::Scalar,
    S_a_monero: monero::PublicKey,
    S_a_bitcoin: bitcoin::PublicKey,
    v: monero::PrivateViewKey,
    xmr: monero::Amount,
    pub cancel_timelock: CancelTimelock,
    punish_timelock: PunishTimelock,
    #[serde(with = "crate::bitcoin::address_serde")]
    refund_address: bitcoin::Address,
    #[serde(with = "crate::bitcoin::address_serde")]
    redeem_address: bitcoin::Address,
    pub tx_lock: bitcoin::TxLock,
    tx_cancel_sig_a: Signature,
    tx_refund_encsig: bitcoin::EncryptedSignature,
    min_monero_confirmations: u64,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_redeem_fee: bitcoin::Amount,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_refund_fee: bitcoin::Amount,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_cancel_fee: bitcoin::Amount,
}

impl State3 {
    pub fn btc_amount(&self) -> bitcoin::Amount {
        self.tx_lock.lock_amount()
    }

    pub fn xmr_amount(&self) -> monero::Amount {
        self.xmr
    }

    pub fn lock_xmr_watch_request(&self, transfer_proof: TransferProof) -> WatchRequest {
        let S_b_monero =
            monero::PublicKey::from_private_key(&monero::PrivateKey::from_scalar(self.s_b));
        let S = self.S_a_monero + S_b_monero;

        WatchRequest {
            public_spend_key: S,
            public_view_key: self.v.public(),
            transfer_proof,
            conf_target: self.min_monero_confirmations,
            expected: self.xmr,
        }
    }

    pub fn xmr_locked(self, monero_wallet_restore_blockheight: BlockHeight) -> State4 {
        State4 {
            A: self.A,
            b: self.b,
            s_b: self.s_b,
            S_a_bitcoin: self.S_a_bitcoin,
            v: self.v,
            cancel_timelock: self.cancel_timelock,
            punish_timelock: self.punish_timelock,
            refund_address: self.refund_address,
            redeem_address: self.redeem_address,
            tx_lock: self.tx_lock,
            tx_cancel_sig_a: self.tx_cancel_sig_a,
            tx_refund_encsig: self.tx_refund_encsig,
            monero_wallet_restore_blockheight,
            tx_redeem_fee: self.tx_redeem_fee,
            tx_refund_fee: self.tx_refund_fee,
            tx_cancel_fee: self.tx_cancel_fee,
        }
    }

    pub fn cancel(&self) -> State6 {
        State6 {
            A: self.A,
            b: self.b.clone(),
            s_b: self.s_b,
            cancel_timelock: self.cancel_timelock,
            punish_timelock: self.punish_timelock,
            refund_address: self.refund_address.clone(),
            tx_lock: self.tx_lock.clone(),
            tx_cancel_sig_a: self.tx_cancel_sig_a.clone(),
            tx_refund_encsig: self.tx_refund_encsig.clone(),
            tx_refund_fee: self.tx_refund_fee,
            tx_cancel_fee: self.tx_cancel_fee,
        }
    }

    pub fn tx_lock_id(&self) -> bitcoin::Txid {
        self.tx_lock.txid()
    }

    pub async fn current_epoch(
        &self,
        bitcoin_wallet: &bitcoin::Wallet,
    ) -> Result<ExpiredTimelocks> {
        let tx_cancel = TxCancel::new(
            &self.tx_lock,
            self.cancel_timelock,
            self.A,
            self.b.public(),
            self.tx_cancel_fee,
        );

        let tx_lock_status = bitcoin_wallet.status_of_script(&self.tx_lock).await?;
        let tx_cancel_status = bitcoin_wallet.status_of_script(&tx_cancel).await?;

        Ok(current_epoch(
            self.cancel_timelock,
            self.punish_timelock,
            tx_lock_status,
            tx_cancel_status,
        ))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct State4 {
    A: bitcoin::PublicKey,
    b: bitcoin::SecretKey,
    s_b: monero::Scalar,
    S_a_bitcoin: bitcoin::PublicKey,
    v: monero::PrivateViewKey,
    pub cancel_timelock: CancelTimelock,
    punish_timelock: PunishTimelock,
    #[serde(with = "crate::bitcoin::address_serde")]
    refund_address: bitcoin::Address,
    #[serde(with = "crate::bitcoin::address_serde")]
    redeem_address: bitcoin::Address,
    pub tx_lock: bitcoin::TxLock,
    tx_cancel_sig_a: Signature,
    tx_refund_encsig: bitcoin::EncryptedSignature,
    monero_wallet_restore_blockheight: BlockHeight,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_redeem_fee: bitcoin::Amount,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_refund_fee: bitcoin::Amount,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_cancel_fee: bitcoin::Amount,
}

impl State4 {
    pub fn btc_amount(&self) -> bitcoin::Amount {
        self.tx_lock.lock_amount()
    }

    pub fn tx_redeem_encsig(&self) -> Result<bitcoin::EncryptedSignature> {
        let tx_redeem =
            bitcoin::TxRedeem::new(&self.tx_lock, &self.redeem_address, self.tx_redeem_fee)?;
        Ok(self.b.encsign(self.S_a_bitcoin, tx_redeem.digest()))
    }

    pub async fn check_for_tx_redeem(&self, bitcoin_wallet: &bitcoin::Wallet) -> Result<State5> {
        let tx_redeem =
            bitcoin::TxRedeem::new(&self.tx_lock, &self.redeem_address, self.tx_redeem_fee)?;
        let tx_redeem_encsig = self.b.encsign(self.S_a_bitcoin, tx_redeem.digest());

        let tx_redeem_candidate = bitcoin_wallet.get_raw_transaction(tx_redeem.txid()).await?;

        let tx_redeem_sig =
            tx_redeem.extract_signature_by_key(tx_redeem_candidate, self.b.public())?;
        let s_a = bitcoin::recover(self.S_a_bitcoin, tx_redeem_sig, tx_redeem_encsig)?;
        let s_a = monero::private_key_from_secp256k1_scalar(s_a.into());

        Ok(State5 {
            s_a,
            s_b: self.s_b,
            v: self.v,
            tx_lock: self.tx_lock.clone(),
            monero_wallet_restore_blockheight: self.monero_wallet_restore_blockheight,
        })
    }

    pub async fn watch_for_redeem_btc(&self, bitcoin_wallet: &bitcoin::Wallet) -> Result<State5> {
        let tx_redeem =
            bitcoin::TxRedeem::new(&self.tx_lock, &self.redeem_address, self.tx_redeem_fee)?;
        let tx_redeem_encsig = self.b.encsign(self.S_a_bitcoin, tx_redeem.digest());

        bitcoin_wallet
            .subscribe_to(tx_redeem.clone())
            .await
            .wait_until_seen()
            .await?;

        let tx_redeem_candidate = bitcoin_wallet.get_raw_transaction(tx_redeem.txid()).await?;

        let tx_redeem_sig =
            tx_redeem.extract_signature_by_key(tx_redeem_candidate, self.b.public())?;
        let s_a = bitcoin::recover(self.S_a_bitcoin, tx_redeem_sig, tx_redeem_encsig)?;
        let s_a = monero::private_key_from_secp256k1_scalar(s_a.into());

        Ok(State5 {
            s_a,
            s_b: self.s_b,
            v: self.v,
            tx_lock: self.tx_lock.clone(),
            monero_wallet_restore_blockheight: self.monero_wallet_restore_blockheight,
        })
    }

    pub async fn expired_timelock(
        &self,
        bitcoin_wallet: &bitcoin::Wallet,
    ) -> Result<ExpiredTimelocks> {
        let tx_cancel = TxCancel::new(
            &self.tx_lock,
            self.cancel_timelock,
            self.A,
            self.b.public(),
            self.tx_cancel_fee,
        );

        let tx_lock_status = bitcoin_wallet.status_of_script(&self.tx_lock).await?;
        let tx_cancel_status = bitcoin_wallet.status_of_script(&tx_cancel).await?;

        Ok(current_epoch(
            self.cancel_timelock,
            self.punish_timelock,
            tx_lock_status,
            tx_cancel_status,
        ))
    }

    pub fn cancel(self) -> State6 {
        State6 {
            A: self.A,
            b: self.b,
            s_b: self.s_b,
            cancel_timelock: self.cancel_timelock,
            punish_timelock: self.punish_timelock,
            refund_address: self.refund_address,
            tx_lock: self.tx_lock,
            tx_cancel_sig_a: self.tx_cancel_sig_a,
            tx_refund_encsig: self.tx_refund_encsig,
            tx_refund_fee: self.tx_refund_fee,
            tx_cancel_fee: self.tx_cancel_fee,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct State5 {
    #[serde(with = "monero_private_key")]
    s_a: monero::PrivateKey,
    s_b: monero::Scalar,
    v: monero::PrivateViewKey,
    tx_lock: bitcoin::TxLock,
    pub monero_wallet_restore_blockheight: BlockHeight,
}

impl State5 {
    pub fn btc_amount(&self) -> bitcoin::Amount {
        self.tx_lock.lock_amount()
    }

    pub fn xmr_keys(&self) -> (monero::PrivateKey, monero::PrivateViewKey) {
        let s_b = monero::PrivateKey { scalar: self.s_b };
        let s = self.s_a + s_b;

        (s, self.v)
    }

    pub fn tx_lock_id(&self) -> bitcoin::Txid {
        self.tx_lock.txid()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct State6 {
    A: bitcoin::PublicKey,
    b: bitcoin::SecretKey,
    s_b: monero::Scalar,
    cancel_timelock: CancelTimelock,
    punish_timelock: PunishTimelock,
    #[serde(with = "crate::bitcoin::address_serde")]
    refund_address: bitcoin::Address,
    tx_lock: bitcoin::TxLock,
    tx_cancel_sig_a: Signature,
    tx_refund_encsig: bitcoin::EncryptedSignature,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    pub tx_refund_fee: bitcoin::Amount,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    pub tx_cancel_fee: bitcoin::Amount,
}

impl State6 {
    pub fn btc_amount(&self) -> bitcoin::Amount {
        self.tx_lock.lock_amount()
    }

    pub async fn expired_timelock(
        &self,
        bitcoin_wallet: &bitcoin::Wallet,
    ) -> Result<ExpiredTimelocks> {
        let tx_cancel = TxCancel::new(
            &self.tx_lock,
            self.cancel_timelock,
            self.A,
            self.b.public(),
            self.tx_cancel_fee,
        );

        let tx_lock_status = bitcoin_wallet.status_of_script(&self.tx_lock).await?;
        let tx_cancel_status = bitcoin_wallet.status_of_script(&tx_cancel).await?;

        Ok(current_epoch(
            self.cancel_timelock,
            self.punish_timelock,
            tx_lock_status,
            tx_cancel_status,
        ))
    }

    pub async fn check_for_tx_cancel(
        &self,
        bitcoin_wallet: &bitcoin::Wallet,
    ) -> Result<Transaction> {
        let tx_cancel = bitcoin::TxCancel::new(
            &self.tx_lock,
            self.cancel_timelock,
            self.A,
            self.b.public(),
            self.tx_cancel_fee,
        );

        let tx = bitcoin_wallet.get_raw_transaction(tx_cancel.txid()).await?;

        Ok(tx)
    }

    pub async fn submit_tx_cancel(
        &self,
        bitcoin_wallet: &bitcoin::Wallet,
    ) -> Result<(Txid, Subscription)> {
        let transaction = bitcoin::TxCancel::new(
            &self.tx_lock,
            self.cancel_timelock,
            self.A,
            self.b.public(),
            self.tx_cancel_fee,
        )
        .complete_as_bob(self.A, self.b.clone(), self.tx_cancel_sig_a.clone())
        .context("Failed to complete Bitcoin cancel transaction")?;

        let (tx_id, subscription) = bitcoin_wallet.broadcast(transaction, "cancel").await?;

        Ok((tx_id, subscription))
    }

    pub async fn cancel_transaction_status(
        &self,
        bitcoin_wallet: &bitcoin::Wallet,
    ) -> Result<crate::bitcoin::wallet::ScriptStatus> {
        let tx_cancel = bitcoin::TxCancel::new(
            &self.tx_lock,
            self.cancel_timelock,
            self.A,
            self.b.public(),
            self.tx_cancel_fee,
        );

        bitcoin_wallet.status_of_script(&tx_cancel).await
    }

    pub async fn publish_refund_btc(&self, bitcoin_wallet: &bitcoin::Wallet) -> Result<()> {
        let signed_tx_refund = self.signed_refund_transaction()?;
        bitcoin_wallet.broadcast(signed_tx_refund, "refund").await?;

        Ok(())
    }

    pub fn signed_refund_transaction(&self) -> Result<Transaction> {
        let tx_cancel = bitcoin::TxCancel::new(
            &self.tx_lock,
            self.cancel_timelock,
            self.A,
            self.b.public(),
            self.tx_cancel_fee,
        );
        let tx_refund =
            bitcoin::TxRefund::new(&tx_cancel, &self.refund_address, self.tx_refund_fee)?;

        let adaptor = Adaptor::<HashTranscript<Sha256>, Deterministic<Sha256>>::default();

        let sig_b = self.b.sign(tx_refund.digest());
        let sig_a =
            adaptor.decrypt_signature(&self.s_b.to_secpfun_scalar(), self.tx_refund_encsig.clone());

        let signed_tx_refund =
            tx_refund.add_signatures((self.A, sig_a), (self.b.public(), sig_b))?;
        Ok(signed_tx_refund)
    }

    pub fn tx_lock_id(&self) -> bitcoin::Txid {
        self.tx_lock.txid()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitcoin::{Amount, TxPunish, TxRedeem, TxRefund, WalletBuilder};
    use crate::env::{GetConfig, Regtest};
    use crate::protocol::alice;
    use ::bitcoin::hashes::Hash;
        use rand::rngs::OsRng;

    /// Drives a fresh Bob/Alice handshake to (Bob State1, Alice State2) — the
    /// point where Alice's Message3 (cancel sig + refund encsig) arrives.
    async fn state1_and_valid_message3() -> (State1, Message3) {
        let alice_wallet = WalletBuilder::new(Amount::ONE_BTC.to_sat()).build();
        let bob_wallet = WalletBuilder::new(Amount::ONE_BTC.to_sat()).build();
        let btc_amount = Amount::from_sat(500_000);
        let xmr_amount = monero::Amount::from_piconero(10_000);
        let config = Regtest::get_config();

        let tx_redeem_fee = alice_wallet
            .estimate_fee_vsize_at(
                TxRedeem::vsize(),
                btc_amount,
                bitcoin::fee_targets::FEE_TARGET_TIME_CRITICAL,
            )
            .await
            .unwrap();
        let tx_punish_fee = alice_wallet
            .estimate_fee_vsize_at(
                TxPunish::vsize(),
                btc_amount,
                bitcoin::fee_targets::FEE_TARGET_TIMELIGHT_BOUND,
            )
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

        let bob_state0 = State0::new(
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
        let alice_state0 = alice::State0::new(
            btc_amount,
            xmr_amount,
            config,
            alice_wallet.new_address().await.unwrap(),
            alice_wallet.new_address().await.unwrap(),
            tx_redeem_fee,
            tx_punish_fee,
            tx_cancel_fee,
            tx_refund_fee,
            &mut OsRng,
        );

        let (_, alice_state1) = alice_state0.receive(bob_state0.next_message()).unwrap();
        let bob_state1 = bob_state0
            .receive(&bob_wallet, alice_state1.next_message())
            .await
            .unwrap();
        let alice_state2 = alice_state1.receive(bob_state1.next_message()).unwrap();
        (bob_state1, alice_state2.next_message().unwrap())
    }

    async fn receive_with_fee_override(
        tx_redeem_fee_override: Option<Amount>,
        tx_punish_fee_override: Option<Amount>,
    ) -> Result<State1> {
        let alice_wallet = WalletBuilder::new(Amount::ONE_BTC.to_sat()).build();
        let bob_wallet = WalletBuilder::new(Amount::ONE_BTC.to_sat()).build();
        let btc_amount = Amount::from_sat(500_000);
        let xmr_amount = monero::Amount::from_piconero(10_000);
        let config = Regtest::get_config();

        let tx_redeem_fee = alice_wallet
            .estimate_fee_vsize_at(
                TxRedeem::vsize(),
                btc_amount,
                bitcoin::fee_targets::FEE_TARGET_TIME_CRITICAL,
            )
            .await
            .unwrap();
        let tx_punish_fee = alice_wallet
            .estimate_fee_vsize_at(
                TxPunish::vsize(),
                btc_amount,
                bitcoin::fee_targets::FEE_TARGET_TIMELIGHT_BOUND,
            )
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

        let bob_state0 = State0::new(
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
        let alice_state0 = alice::State0::new(
            btc_amount,
            xmr_amount,
            config,
            alice_wallet.new_address().await.unwrap(),
            alice_wallet.new_address().await.unwrap(),
            tx_redeem_fee,
            tx_punish_fee,
            tx_cancel_fee,
            tx_refund_fee,
            &mut OsRng,
        );

        let (_, alice_state1) = alice_state0.receive(bob_state0.next_message()).unwrap();
        let mut message1 = alice_state1.next_message();
        if let Some(tx_redeem_fee) = tx_redeem_fee_override {
            message1.tx_redeem_fee = tx_redeem_fee;
        }
        if let Some(tx_punish_fee) = tx_punish_fee_override {
            message1.tx_punish_fee = tx_punish_fee;
        }

        bob_state0.receive(&bob_wallet, message1).await
    }

    #[tokio::test]
    async fn alice_proposing_a_griefing_redeem_fee_is_rejected() {
        let btc_amount = Amount::from_sat(500_000);

        let error = receive_with_fee_override(Some(btc_amount), None)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("tx_redeem_fee"),
            "expected a tx_redeem_fee sanity-check error, got: {error:#}"
        );
    }

    #[tokio::test]
    async fn alice_proposing_a_griefing_punish_fee_is_rejected() {
        let btc_amount = Amount::from_sat(500_000);

        let error = receive_with_fee_override(None, Some(btc_amount))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("tx_punish_fee"),
            "expected a tx_punish_fee sanity-check error, got: {error:#}"
        );
    }

    #[tokio::test]
    async fn alice_proposing_a_dust_redeem_fee_is_rejected() {
        let error = receive_with_fee_override(Some(Amount::from_sat(1)), None)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("tx_redeem_fee"),
            "expected a tx_redeem_fee sanity-check error, got: {error:#}"
        );
    }

    #[tokio::test]
    async fn alice_honest_fees_are_accepted() {
        receive_with_fee_override(None, None).await.unwrap();
    }

    #[tokio::test]
    async fn alice_message1_with_dleq_proof_for_wrong_scalar_is_rejected() {
        let alice_wallet = WalletBuilder::new(Amount::ONE_BTC.to_sat()).build();
        let bob_wallet = WalletBuilder::new(Amount::ONE_BTC.to_sat()).build();
        let btc_amount = Amount::from_sat(500_000);
        let xmr_amount = monero::Amount::from_piconero(10_000);
        let config = Regtest::get_config();
        let tx_cancel_fee = bob_wallet
            .estimate_fee_vsize(TxCancel::vsize(), btc_amount)
            .await
            .unwrap();
        let tx_refund_fee = bob_wallet
            .estimate_fee_vsize(TxRefund::vsize(), btc_amount)
            .await
            .unwrap();

        let bob_state0 = State0::new(
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
        let alice_state0 = alice::State0::new(
            btc_amount,
            xmr_amount,
            config,
            alice_wallet.new_address().await.unwrap(),
            alice_wallet.new_address().await.unwrap(),
            alice_wallet
                .estimate_fee(TxRedeem::weight(), btc_amount)
                .await
                .unwrap(),
            alice_wallet
                .estimate_fee(TxPunish::weight(), btc_amount)
                .await
                .unwrap(),
            alice_wallet
                .estimate_fee_vsize(TxCancel::vsize(), btc_amount)
                .await
                .unwrap(),
            alice_wallet
                .estimate_fee_vsize(TxRefund::vsize(), btc_amount)
                .await
                .unwrap(),
            &mut OsRng,
        );
        let (_, alice_state1) = alice_state0.receive(bob_state0.next_message()).unwrap();

        // A valid DLEQ proof for a scalar other than the one behind the
        // advertised keys must not verify.
        let mut alice_message1 = alice_state1.next_message();
        let (wrong_proof, _) =
            CROSS_CURVE_PROOF_SYSTEM.prove(&monero::Scalar::random(&mut OsRng), &mut OsRng);
        alice_message1.dleq_proof_s_a = wrong_proof;

        assert!(bob_state0
            .receive(&bob_wallet, alice_message1)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn alice_message3_with_forged_cancel_signature_is_rejected() {
        let (bob_state1, mut message3) = state1_and_valid_message3().await;

        // A signature by the wrong key over a different digest must not verify.
        message3.tx_cancel_sig = bitcoin::SecretKey::new_random(&mut OsRng)
            .sign([1u8; 32]);

        assert!(bob_state1.receive(message3).is_err());
    }

    #[tokio::test]
    async fn alice_message3_with_forged_refund_encsig_is_rejected() {
        let (bob_state1, mut message3) = state1_and_valid_message3().await;

        // Encrypted signature by the wrong key must not verify.
        message3.tx_refund_encsig = bitcoin::SecretKey::new_random(&mut OsRng).encsign(
            bitcoin::SecretKey::new_random(&mut OsRng).public(),
            [2u8; 32],
        );

        assert!(bob_state1.receive(message3).is_err());
    }

    #[tokio::test]
    async fn bob_message4_signatures_verify_against_cancel_and_punish_digests() {
        let (bob_state1, message3) = state1_and_valid_message3().await;
        let bob_state2 = bob_state1.receive(message3).unwrap();
        let message4 = bob_state2.next_message().unwrap();

        let tx_cancel = TxCancel::new(
            &bob_state2.tx_lock,
            bob_state2.cancel_timelock,
            bob_state2.A,
            bob_state2.b.public(),
            bob_state2.tx_cancel_fee,
        );
        // Both parties sign the cancel tx: Alice's sig arrived in Message3
        // (verified in State1::receive against A), Bob's is in Message4.
        bitcoin::verify_sig(
            &bob_state2.b.public(),
            &tx_cancel.digest(),
            &message4.tx_cancel_sig,
        )
        .unwrap();

        let tx_punish = TxPunish::new(
            &tx_cancel,
            &bob_state2.punish_address,
            bob_state2.punish_timelock,
            bob_state2.tx_punish_fee,
        )
        .unwrap();
        bitcoin::verify_sig(
            &bob_state2.b.public(),
            &tx_punish.digest(),
            &message4.tx_punish_sig,
        )
        .unwrap();
    }
}
