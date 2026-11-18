use crate::bitcoin::{
    current_epoch, CancelTimelock, ExpiredTimelocks, PunishTimelock, Transaction, TxCancel,
    TxPunish, TxRedeem, TxRefund, Txid,
};
use crate::env::Config;
use crate::monero::wallet::{TransferRequest, WatchRequest};
use crate::monero::TransferProof;
use crate::monero_ext::ScalarExt;
use crate::protocol::{Message0, Message1, Message2, Message3, Message4, CROSS_CURVE_PROOF_SYSTEM};
use crate::{bitcoin, monero};
use anyhow::{anyhow, bail, Context, Result};
use monero_rpc::wallet::BlockHeight;
use rand::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use sigma_fun::ext::dl_secp256k1_ed25519_eq::CrossCurveDLEQProof;
use std::fmt;
use std::time::Duration;
use uuid::Uuid;

// A safety bound, not an expected wait: Bitcoin blocks can be slow and a
// punish transaction must survive normal fee-market delays. After this,
// recovery can retry instead of keeping the task non-terminal forever.
const PUNISH_FINALITY_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq)]
pub enum AliceState {
    Started {
        state3: Box<State3>,
    },
    BtcLockTransactionSeen {
        state3: Box<State3>,
    },
    BtcLocked {
        state3: Box<State3>,
    },
    XmrLockTransactionSent {
        monero_wallet_restore_blockheight: BlockHeight,
        transfer_proof: TransferProof,
        state3: Box<State3>,
    },
    XmrLocked {
        monero_wallet_restore_blockheight: BlockHeight,
        transfer_proof: TransferProof,
        state3: Box<State3>,
    },
    XmrLockTransferProofSent {
        monero_wallet_restore_blockheight: BlockHeight,
        transfer_proof: TransferProof,
        state3: Box<State3>,
    },
    EncSigLearned {
        monero_wallet_restore_blockheight: BlockHeight,
        transfer_proof: TransferProof,
        encrypted_signature: Box<bitcoin::EncryptedSignature>,
        state3: Box<State3>,
    },
    BtcRedeemTransactionPublished {
        state3: Box<State3>,
    },
    BtcRedeemed,
    BtcCancelled {
        monero_wallet_restore_blockheight: BlockHeight,
        transfer_proof: TransferProof,
        state3: Box<State3>,
    },
    BtcRefunded {
        monero_wallet_restore_blockheight: BlockHeight,
        transfer_proof: TransferProof,
        spend_key: monero::PrivateKey,
        state3: Box<State3>,
    },
    BtcPunishable {
        monero_wallet_restore_blockheight: BlockHeight,
        transfer_proof: TransferProof,
        state3: Box<State3>,
    },
    XmrRefunded,
    CancelTimelockExpired {
        monero_wallet_restore_blockheight: BlockHeight,
        transfer_proof: TransferProof,
        state3: Box<State3>,
    },
    BtcPunished,
    SafelyAborted,
}

impl fmt::Display for AliceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AliceState::Started { .. } => write!(f, "started"),
            AliceState::BtcLockTransactionSeen { .. } => {
                write!(f, "bitcoin lock transaction in mempool")
            }
            AliceState::BtcLocked { .. } => write!(f, "btc is locked"),
            AliceState::XmrLockTransactionSent { .. } => write!(f, "xmr lock transaction sent"),
            AliceState::XmrLocked { .. } => write!(f, "xmr is locked"),
            AliceState::XmrLockTransferProofSent { .. } => {
                write!(f, "xmr lock transfer proof sent")
            }
            AliceState::EncSigLearned { .. } => write!(f, "encrypted signature is learned"),
            AliceState::BtcRedeemTransactionPublished { .. } => {
                write!(f, "bitcoin redeem transaction published")
            }
            AliceState::BtcRedeemed => write!(f, "btc is redeemed"),
            AliceState::BtcCancelled { .. } => write!(f, "btc is cancelled"),
            AliceState::BtcRefunded { .. } => write!(f, "btc is refunded"),
            AliceState::BtcPunished => write!(f, "btc is punished"),
            AliceState::SafelyAborted => write!(f, "safely aborted"),
            AliceState::BtcPunishable { .. } => write!(f, "btc is punishable"),
            AliceState::XmrRefunded => write!(f, "xmr is refunded"),
            AliceState::CancelTimelockExpired { .. } => write!(f, "cancel timelock is expired"),
        }
    }
}

impl AliceState {
    /// For states that have not yet locked any XMR, the [`State3`] carrying
    /// the amount a resumed swap will eventually lock. Used to re-reserve
    /// liquidity at startup.
    pub fn pre_xmr_lock_state3(&self) -> Option<&State3> {
        match self {
            AliceState::Started { state3 }
            | AliceState::BtcLockTransactionSeen { state3 }
            | AliceState::BtcLocked { state3 } => Some(state3),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct State0 {
    a: bitcoin::SecretKey,
    s_a: monero::Scalar,
    v_a: monero::PrivateViewKey,
    S_a_monero: monero::PublicKey,
    S_a_bitcoin: bitcoin::PublicKey,
    dleq_proof_s_a: CrossCurveDLEQProof,
    btc: bitcoin::Amount,
    xmr: monero::Amount,
    cancel_timelock: CancelTimelock,
    punish_timelock: PunishTimelock,
    redeem_address: bitcoin::Address,
    punish_address: bitcoin::Address,
    tx_redeem_fee: bitcoin::Amount,
    tx_punish_fee: bitcoin::Amount,
    // Our own conservative fee estimates for the transactions whose fees Bob proposes in
    // Message0 (tx_cancel_fee, tx_refund_fee). Used to sanity-check his proposal in
    // `receive` - never trust a peer-supplied fee outright, see `bitcoin::sanity_check_transaction_fee`.
    own_tx_cancel_fee_estimate: bitcoin::Amount,
    own_tx_refund_fee_estimate: bitcoin::Amount,
}

impl State0 {
    #[allow(clippy::too_many_arguments)]
    pub fn new<R>(
        btc: bitcoin::Amount,
        xmr: monero::Amount,
        env_config: Config,
        redeem_address: bitcoin::Address,
        punish_address: bitcoin::Address,
        tx_redeem_fee: bitcoin::Amount,
        tx_punish_fee: bitcoin::Amount,
        own_tx_cancel_fee_estimate: bitcoin::Amount,
        own_tx_refund_fee_estimate: bitcoin::Amount,
        rng: &mut R,
    ) -> Self
    where
        R: RngCore + CryptoRng,
    {
        let a = bitcoin::SecretKey::new_random(rng);
        let v_a = monero::PrivateViewKey::new_random(rng);

        let s_a = monero::Scalar::random(rng);
        let (dleq_proof_s_a, (S_a_bitcoin, S_a_monero)) = CROSS_CURVE_PROOF_SYSTEM.prove(&s_a, rng);

        Self {
            a,
            s_a,
            v_a,
            S_a_bitcoin: S_a_bitcoin.into(),
            S_a_monero: monero::PublicKey {
                point: S_a_monero.compress(),
            },
            dleq_proof_s_a,
            redeem_address,
            punish_address,
            btc,
            xmr,
            cancel_timelock: env_config.bitcoin_cancel_timelock,
            punish_timelock: env_config.bitcoin_punish_timelock,
            tx_redeem_fee,
            tx_punish_fee,
            own_tx_cancel_fee_estimate,
            own_tx_refund_fee_estimate,
        }
    }

    pub fn receive(self, msg: Message0) -> Result<(Uuid, State1)> {
        bitcoin::sanity_check_transaction_fee(msg.tx_cancel_fee, self.own_tx_cancel_fee_estimate)
            .context("Bob's proposed tx_cancel_fee failed sanity check")?;
        bitcoin::sanity_check_transaction_fee(msg.tx_refund_fee, self.own_tx_refund_fee_estimate)
            .context("Bob's proposed tx_refund_fee failed sanity check")?;
        bitcoin::sanity_check_combined_fees(msg.tx_cancel_fee, msg.tx_refund_fee, self.btc)
            .context("Bob's combined cancel/refund fees failed the output check")?;

        let valid = CROSS_CURVE_PROOF_SYSTEM.verify(
            &msg.dleq_proof_s_b,
            (
                msg.S_b_bitcoin.into(),
                msg.S_b_monero
                    .point
                    .decompress()
                    .ok_or_else(|| anyhow!("S_b is not a monero curve point"))?,
            ),
        );

        if !valid {
            bail!("Bob's dleq proof doesn't verify")
        }

        let v = self.v_a + msg.v_b;

        Ok((
            msg.swap_id,
            State1 {
                a: self.a,
                B: msg.B,
                s_a: self.s_a,
                S_a_monero: self.S_a_monero,
                S_a_bitcoin: self.S_a_bitcoin,
                S_b_monero: msg.S_b_monero,
                S_b_bitcoin: msg.S_b_bitcoin,
                v,
                v_a: self.v_a,
                dleq_proof_s_a: self.dleq_proof_s_a,
                btc: self.btc,
                xmr: self.xmr,
                cancel_timelock: self.cancel_timelock,
                punish_timelock: self.punish_timelock,
                refund_address: msg.refund_address,
                redeem_address: self.redeem_address,
                punish_address: self.punish_address,
                tx_redeem_fee: self.tx_redeem_fee,
                tx_punish_fee: self.tx_punish_fee,
                tx_refund_fee: msg.tx_refund_fee,
                tx_cancel_fee: msg.tx_cancel_fee,
            },
        ))
    }
}

#[derive(Clone, Debug)]
pub struct State1 {
    a: bitcoin::SecretKey,
    B: bitcoin::PublicKey,
    s_a: monero::Scalar,
    S_a_monero: monero::PublicKey,
    S_a_bitcoin: bitcoin::PublicKey,
    S_b_monero: monero::PublicKey,
    S_b_bitcoin: bitcoin::PublicKey,
    v: monero::PrivateViewKey,
    v_a: monero::PrivateViewKey,
    dleq_proof_s_a: CrossCurveDLEQProof,
    btc: bitcoin::Amount,
    xmr: monero::Amount,
    cancel_timelock: CancelTimelock,
    punish_timelock: PunishTimelock,
    refund_address: bitcoin::Address,
    redeem_address: bitcoin::Address,
    punish_address: bitcoin::Address,
    tx_redeem_fee: bitcoin::Amount,
    tx_punish_fee: bitcoin::Amount,
    tx_refund_fee: bitcoin::Amount,
    tx_cancel_fee: bitcoin::Amount,
}

impl State1 {
    pub fn next_message(&self) -> Message1 {
        Message1 {
            A: self.a.public(),
            S_a_monero: self.S_a_monero,
            S_a_bitcoin: self.S_a_bitcoin,
            dleq_proof_s_a: self.dleq_proof_s_a.clone(),
            v_a: self.v_a,
            redeem_address: self.redeem_address.clone(),
            punish_address: self.punish_address.clone(),
            tx_redeem_fee: self.tx_redeem_fee,
            tx_punish_fee: self.tx_punish_fee,
        }
    }

    pub fn receive(self, msg: Message2) -> Result<State2> {
        let tx_lock = bitcoin::TxLock::from_psbt(msg.psbt, self.a.public(), self.B, self.btc)
            .context("Failed to re-construct TxLock from received PSBT")?;

        Ok(State2 {
            a: self.a,
            B: self.B,
            s_a: self.s_a,
            S_b_monero: self.S_b_monero,
            S_b_bitcoin: self.S_b_bitcoin,
            v: self.v,
            btc: self.btc,
            xmr: self.xmr,
            cancel_timelock: self.cancel_timelock,
            punish_timelock: self.punish_timelock,
            refund_address: self.refund_address,
            redeem_address: self.redeem_address,
            punish_address: self.punish_address,
            tx_lock,
            tx_redeem_fee: self.tx_redeem_fee,
            tx_punish_fee: self.tx_punish_fee,
            tx_refund_fee: self.tx_refund_fee,
            tx_cancel_fee: self.tx_cancel_fee,
        })
    }
}

#[derive(Clone, Debug)]
pub struct State2 {
    a: bitcoin::SecretKey,
    B: bitcoin::PublicKey,
    s_a: monero::Scalar,
    S_b_monero: monero::PublicKey,
    S_b_bitcoin: bitcoin::PublicKey,
    v: monero::PrivateViewKey,
    btc: bitcoin::Amount,
    xmr: monero::Amount,
    cancel_timelock: CancelTimelock,
    punish_timelock: PunishTimelock,
    refund_address: bitcoin::Address,
    redeem_address: bitcoin::Address,
    punish_address: bitcoin::Address,
    tx_lock: bitcoin::TxLock,
    tx_redeem_fee: bitcoin::Amount,
    tx_punish_fee: bitcoin::Amount,
    tx_refund_fee: bitcoin::Amount,
    tx_cancel_fee: bitcoin::Amount,
}

impl State2 {
    pub fn next_message(&self) -> Result<Message3> {
        let tx_cancel = bitcoin::TxCancel::new(
            &self.tx_lock,
            self.cancel_timelock,
            self.a.public(),
            self.B,
            self.tx_cancel_fee,
        );

        let tx_refund =
            bitcoin::TxRefund::new(&tx_cancel, &self.refund_address, self.tx_refund_fee)?;
        // Alice encsigns the refund transaction(bitcoin) digest with Bob's monero
        // pubkey(S_b). The refund transaction spends the output of
        // tx_lock_bitcoin to Bob's refund address.
        // recover(encsign(a, S_b, d), sign(a, d), S_b) = s_b where d is a digest, (a,
        // A) is alice's keypair and (s_b, S_b) is bob's keypair.
        let tx_refund_encsig = self.a.encsign(self.S_b_bitcoin, tx_refund.digest());

        let tx_cancel_sig = self.a.sign(tx_cancel.digest());
        Ok(Message3 {
            tx_cancel_sig,
            tx_refund_encsig,
        })
    }

    pub fn receive(self, msg: Message4) -> Result<State3> {
        let tx_cancel = bitcoin::TxCancel::new(
            &self.tx_lock,
            self.cancel_timelock,
            self.a.public(),
            self.B,
            self.tx_cancel_fee,
        );
        bitcoin::verify_sig(&self.B, &tx_cancel.digest(), &msg.tx_cancel_sig)
            .context("Failed to verify cancel transaction")?;
        let tx_punish = bitcoin::TxPunish::new(
            &tx_cancel,
            &self.punish_address,
            self.punish_timelock,
            self.tx_punish_fee,
        )?;
        bitcoin::verify_sig(&self.B, &tx_punish.digest(), &msg.tx_punish_sig)
            .context("Failed to verify punish transaction")?;

        Ok(State3 {
            a: self.a,
            B: self.B,
            s_a: self.s_a,
            S_b_monero: self.S_b_monero,
            S_b_bitcoin: self.S_b_bitcoin,
            v: self.v,
            btc: self.btc,
            xmr: self.xmr,
            cancel_timelock: self.cancel_timelock,
            punish_timelock: self.punish_timelock,
            refund_address: self.refund_address,
            redeem_address: self.redeem_address,
            punish_address: self.punish_address,
            tx_lock: self.tx_lock,
            tx_punish_sig_bob: msg.tx_punish_sig,
            tx_cancel_sig_bob: msg.tx_cancel_sig,
            tx_redeem_fee: self.tx_redeem_fee,
            tx_punish_fee: self.tx_punish_fee,
            tx_refund_fee: self.tx_refund_fee,
            tx_cancel_fee: self.tx_cancel_fee,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct State3 {
    a: bitcoin::SecretKey,
    B: bitcoin::PublicKey,
    s_a: monero::Scalar,
    S_b_monero: monero::PublicKey,
    S_b_bitcoin: bitcoin::PublicKey,
    pub v: monero::PrivateViewKey,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    btc: bitcoin::Amount,
    xmr: monero::Amount,
    pub cancel_timelock: CancelTimelock,
    pub punish_timelock: PunishTimelock,
    #[serde(with = "crate::bitcoin::address_serde")]
    refund_address: bitcoin::Address,
    #[serde(with = "crate::bitcoin::address_serde")]
    redeem_address: bitcoin::Address,
    #[serde(with = "crate::bitcoin::address_serde")]
    punish_address: bitcoin::Address,
    pub tx_lock: bitcoin::TxLock,
    tx_punish_sig_bob: bitcoin::Signature,
    tx_cancel_sig_bob: bitcoin::Signature,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_redeem_fee: bitcoin::Amount,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_punish_fee: bitcoin::Amount,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_refund_fee: bitcoin::Amount,
    #[serde(with = "::bitcoin::amount::serde::as_sat")]
    tx_cancel_fee: bitcoin::Amount,
}

impl State3 {
    /// The Monero amount this swap will lock once it proceeds.
    pub fn xmr_amount(&self) -> monero::Amount {
        self.xmr
    }

    pub async fn expired_timelocks(
        &self,
        bitcoin_wallet: &bitcoin::Wallet,
    ) -> Result<ExpiredTimelocks> {
        let tx_cancel = self.tx_cancel();

        let tx_lock_status = bitcoin_wallet.status_of_script(&self.tx_lock).await?;
        let tx_cancel_status = bitcoin_wallet.status_of_script(&tx_cancel).await?;

        Ok(current_epoch(
            self.cancel_timelock,
            self.punish_timelock,
            tx_lock_status,
            tx_cancel_status,
        ))
    }

    pub fn lock_xmr_transfer_request(&self) -> TransferRequest {
        let S_a = monero::PublicKey::from_private_key(&monero::PrivateKey { scalar: self.s_a });

        let public_spend_key = S_a + self.S_b_monero;
        let public_view_key = self.v.public();

        TransferRequest {
            public_spend_key,
            public_view_key,
            amount: self.xmr,
        }
    }

    pub fn lock_xmr_watch_request(
        &self,
        transfer_proof: TransferProof,
        conf_target: u64,
    ) -> WatchRequest {
        let S_a = monero::PublicKey::from_private_key(&monero::PrivateKey { scalar: self.s_a });

        let public_spend_key = S_a + self.S_b_monero;
        let public_view_key = self.v.public();
        WatchRequest {
            public_spend_key,
            public_view_key,
            transfer_proof,
            conf_target,
            expected: self.xmr,
        }
    }

    pub fn tx_cancel(&self) -> TxCancel {
        TxCancel::new(
            &self.tx_lock,
            self.cancel_timelock,
            self.a.public(),
            self.B,
            self.tx_cancel_fee,
        )
    }

    pub fn tx_refund(&self) -> Result<TxRefund> {
        bitcoin::TxRefund::new(&self.tx_cancel(), &self.refund_address, self.tx_refund_fee)
    }

    pub fn tx_redeem(&self) -> Result<TxRedeem> {
        TxRedeem::new(&self.tx_lock, &self.redeem_address, self.tx_redeem_fee)
    }

    pub fn extract_monero_private_key(
        &self,
        published_refund_tx: bitcoin::Transaction,
    ) -> Result<monero::PrivateKey> {
        self.tx_refund()?.extract_monero_private_key(
            published_refund_tx,
            self.s_a,
            self.a.clone(),
            self.S_b_bitcoin,
        )
    }

    pub async fn check_for_tx_cancel(
        &self,
        bitcoin_wallet: &bitcoin::Wallet,
    ) -> Result<Transaction> {
        let tx_cancel = self.tx_cancel();
        let tx = bitcoin_wallet.get_raw_transaction(tx_cancel.txid()).await?;
        Ok(tx)
    }

    pub async fn fetch_tx_refund(&self, bitcoin_wallet: &bitcoin::Wallet) -> Result<Transaction> {
        let tx_refund = self.tx_refund()?;
        let tx = bitcoin_wallet.get_raw_transaction(tx_refund.txid()).await?;
        Ok(tx)
    }

    pub async fn submit_tx_cancel(&self, bitcoin_wallet: &bitcoin::Wallet) -> Result<Txid> {
        let transaction = self.signed_cancel_transaction()?;
        let (tx_id, _) = bitcoin_wallet.broadcast(transaction, "cancel").await?;
        Ok(tx_id)
    }

    pub async fn refund_xmr(
        &self,
        monero_wallet: &monero::Wallet,
        monero_wallet_restore_blockheight: BlockHeight,
        file_name: String,
        spend_key: monero::PrivateKey,
        transfer_proof: TransferProof,
    ) -> Result<()> {
        let view_key = self.v;

        // Ensure that the XMR to be refunded are spendable by awaiting 10 confirmations
        // on the lock transaction
        monero_wallet
            .watch_for_transfer(self.lock_xmr_watch_request(transfer_proof, 10))
            .await?;

        monero_wallet
            .create_from(
                file_name,
                spend_key,
                view_key,
                monero_wallet_restore_blockheight,
            )
            .await?;

        Ok(())
    }

    pub async fn punish_btc(&self, bitcoin_wallet: &bitcoin::Wallet) -> Result<Txid> {
        let signed_tx_punish = self.signed_punish_transaction()?;

        let (txid, subscription) = bitcoin_wallet.broadcast(signed_tx_punish, "punish").await?;
        tokio::time::timeout(PUNISH_FINALITY_TIMEOUT, subscription.wait_until_final())
            .await
            .context("Timed out waiting for the punish transaction to become final")??;

        Ok(txid)
    }

    pub fn signed_redeem_transaction(
        &self,
        sig: bitcoin::EncryptedSignature,
    ) -> Result<bitcoin::Transaction> {
        bitcoin::TxRedeem::new(&self.tx_lock, &self.redeem_address, self.tx_redeem_fee)?
            .complete(sig, self.a.clone(), self.s_a.to_secpfun_scalar(), self.B)
            .context("Failed to complete Bitcoin redeem transaction")
    }

    pub fn signed_cancel_transaction(&self) -> Result<bitcoin::Transaction> {
        self.tx_cancel()
            .complete_as_alice(self.a.clone(), self.B, self.tx_cancel_sig_bob.clone())
            .context("Failed to complete Bitcoin cancel transaction")
    }

    pub fn signed_punish_transaction(&self) -> Result<bitcoin::Transaction> {
        self.tx_punish()?
            .complete(self.tx_punish_sig_bob.clone(), self.a.clone(), self.B)
            .context("Failed to complete Bitcoin punish transaction")
    }

    fn tx_punish(&self) -> Result<TxPunish> {
        bitcoin::TxPunish::new(
            &self.tx_cancel(),
            &self.punish_address,
            self.punish_timelock,
            self.tx_punish_fee,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitcoin::WalletBuilder;
    use crate::env::{GetConfig, Regtest};
    use crate::protocol::bob;
    use rand::rngs::OsRng;

    /// Reproduces the fee-griefing scenario from the security review: Bob proposes a
    /// `tx_cancel_fee`/`tx_refund_fee` far outside Alice's own conservative estimate, which
    /// (pre-fix) would be accepted verbatim and later underflow `lock_amount - fee` when
    /// TxCancel/TxRefund are built - stranding Alice's already-locked Monero with no valid
    /// cancel/refund/punish path. Bob's proposal must now be rejected before Alice transitions
    /// out of State0.
    #[tokio::test]
    async fn bob_proposing_a_griefing_fee_is_rejected() {
        let alice_wallet = WalletBuilder::new(bitcoin::Amount::ONE_BTC.to_sat()).build();
        let bob_wallet = WalletBuilder::new(bitcoin::Amount::ONE_BTC.to_sat()).build();
        let btc_amount = bitcoin::Amount::from_sat(500_000);
        let xmr_amount = monero::Amount::from_piconero(10_000);

        let tx_redeem_fee = alice_wallet
            .estimate_fee(bitcoin::TxRedeem::weight(), btc_amount)
            .await
            .unwrap();
        let tx_punish_fee = alice_wallet
            .estimate_fee(bitcoin::TxPunish::weight(), btc_amount)
            .await
            .unwrap();
        let own_tx_cancel_fee_estimate = alice_wallet
            .estimate_fee_vsize(bitcoin::TxCancel::vsize(), btc_amount)
            .await
            .unwrap();
        let own_tx_refund_fee_estimate = alice_wallet
            .estimate_fee_vsize(bitcoin::TxRefund::vsize(), btc_amount)
            .await
            .unwrap();
        let redeem_address = alice_wallet.new_address().await.unwrap();
        let punish_address = alice_wallet.new_address().await.unwrap();

        let config = Regtest::get_config();

        let new_alice_state0 = || {
            State0::new(
                btc_amount,
                xmr_amount,
                config,
                redeem_address.clone(),
                punish_address.clone(),
                tx_redeem_fee,
                tx_punish_fee,
                own_tx_cancel_fee_estimate,
                own_tx_refund_fee_estimate,
                &mut OsRng,
            )
        };

        // A legitimate Bob proposal, produced by the real bob::State0 so it carries a valid
        // DLEQ proof and well-formed keys - only the fee fields are attacker-manipulated below.
        let bob_state0 = bob::State0::new(
            Uuid::new_v4(),
            &mut OsRng,
            btc_amount,
            xmr_amount,
            config.bitcoin_cancel_timelock,
            config.bitcoin_punish_timelock,
            bob_wallet.new_address().await.unwrap(),
            config.monero_finality_confirmations,
            own_tx_refund_fee_estimate,
            own_tx_cancel_fee_estimate,
        );
        let honest_message0 = bob_state0.next_message();

        // Griefing: propose a tx_cancel_fee close to the whole lock amount.
        let error = new_alice_state0()
            .receive(Message0 {
                tx_cancel_fee: btc_amount,
                ..honest_message0.clone()
            })
            .unwrap_err();
        assert!(
            error.to_string().contains("tx_cancel_fee"),
            "expected a tx_cancel_fee sanity-check error, got: {error:#}"
        );

        // Griefing via tx_refund_fee instead.
        let error = new_alice_state0()
            .receive(Message0 {
                tx_refund_fee: btc_amount,
                ..honest_message0.clone()
            })
            .unwrap_err();
        assert!(
            error.to_string().contains("tx_refund_fee"),
            "expected a tx_refund_fee sanity-check error, got: {error:#}"
        );

        // The floor bound is enforced too, not just the overpay ceiling.
        let error = new_alice_state0()
            .receive(Message0 {
                tx_cancel_fee: bitcoin::Amount::from_sat(1),
                ..honest_message0.clone()
            })
            .unwrap_err();
        assert!(
            error.to_string().contains("tx_cancel_fee"),
            "expected a tx_cancel_fee sanity-check error for a dust fee, got: {error:#}"
        );

        // Each fee can individually pass the 3x ceiling while their sum is
        // larger than the lock output; the combined check must reject it.
        let small_btc_amount = bitcoin::Amount::from_sat(1_000);
        let own_small_fee_estimate = bitcoin::Amount::from_sat(300);
        let alice_state0_small = State0::new(
            small_btc_amount,
            xmr_amount,
            config,
            redeem_address.clone(),
            punish_address.clone(),
            tx_redeem_fee,
            tx_punish_fee,
            own_small_fee_estimate,
            own_small_fee_estimate,
            &mut OsRng,
        );
        let bob_state0_small = bob::State0::new(
            Uuid::new_v4(),
            &mut OsRng,
            small_btc_amount,
            xmr_amount,
            config.bitcoin_cancel_timelock,
            config.bitcoin_punish_timelock,
            bob_wallet.new_address().await.unwrap(),
            config.monero_finality_confirmations,
            bitcoin::Amount::from_sat(600),
            bitcoin::Amount::from_sat(600),
        );
        let error = alice_state0_small
            .receive(bob_state0_small.next_message())
            .unwrap_err();
        assert!(
            error.to_string().contains("combined"),
            "expected a combined cancel/refund fee error, got: {error:#}"
        );

        // A fee close to Alice's own estimate is accepted.
        new_alice_state0().receive(honest_message0).unwrap();
    }

    #[tokio::test]
    async fn bob_fee_proposals_at_current_target_policies_pass_the_sanity_check() {
        let wallet = WalletBuilder::new(bitcoin::Amount::ONE_BTC.to_sat())
            .build_with_fee_strategy(WalletBuilder::target_aware_fee_rate());
        let amount = bitcoin::Amount::from_sat(500_000);

        let alice_cancel_estimate = wallet
            .estimate_fee_vsize_at(
                TxCancel::vsize(),
                amount,
                bitcoin::fee_targets::FEE_TARGET_PRINCIPAL_CRITICAL,
            )
            .await
            .unwrap();
        let bob_crate_cancel_fee = wallet
            .estimate_fee_vsize_at(
                TxCancel::vsize(),
                amount,
                bitcoin::fee_targets::FEE_TARGET_PRINCIPAL_CRITICAL,
            )
            .await
            .unwrap();
        let bob_gui_cancel_fee = wallet
            .estimate_fee_vsize_at_with_rate_multiplier(
                TxCancel::vsize(),
                amount,
                bitcoin::fee_targets::FEE_TARGET_TIME_CRITICAL,
                2.0,
            )
            .await
            .unwrap();

        bitcoin::sanity_check_transaction_fee(bob_crate_cancel_fee, alice_cancel_estimate)
            .expect("the swap crate's principal-critical proposal must pass");
        bitcoin::sanity_check_transaction_fee(bob_gui_cancel_fee, alice_cancel_estimate)
            .expect("the GUI's buffered time-critical proposal must pass");

        let alice_refund_estimate = wallet
            .estimate_fee_vsize_at(
                TxRefund::vsize(),
                amount,
                bitcoin::fee_targets::FEE_TARGET_PRINCIPAL_CRITICAL,
            )
            .await
            .unwrap();
        let bob_crate_refund_fee = wallet
            .estimate_fee_vsize_at(
                TxRefund::vsize(),
                amount,
                bitcoin::fee_targets::FEE_TARGET_PRINCIPAL_CRITICAL,
            )
            .await
            .unwrap();
        let bob_gui_refund_fee = wallet
            .estimate_fee_vsize_at_with_rate_multiplier(
                TxRefund::vsize(),
                amount,
                bitcoin::fee_targets::FEE_TARGET_TIME_CRITICAL,
                2.0,
            )
            .await
            .unwrap();

        bitcoin::sanity_check_transaction_fee(bob_crate_refund_fee, alice_refund_estimate)
            .expect("the swap crate's principal-critical proposal must pass");
        bitcoin::sanity_check_transaction_fee(bob_gui_refund_fee, alice_refund_estimate)
            .expect("the GUI's buffered time-critical proposal must pass");
    }
}
