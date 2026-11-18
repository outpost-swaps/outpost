use crate::bitcoin::wallet::Watchable;
use ::bitcoin::ScriptBuf;
use crate::bitcoin::{
    verify_encsig, verify_sig, Address, Amount, EmptyWitnessStack, EncryptedSignature, NoInputs,
    NotThreeWitnesses, PublicKey, SecretKey, TooManyInputs, Transaction, TxLock,
};
use crate::bitcoin::Sighash;
use ::bitcoin::Txid;
use ::bitcoin::hashes::Hash;
use anyhow::{bail, Context, Result};
use miniscript::Descriptor;
use bitcoin::secp256k1::ecdsa;
use bitcoin::sighash::SighashCache;
use bitcoin::{EcdsaSighashType, Script};
use ecdsa_fun::adaptor::{Adaptor, HashTranscript};
use ecdsa_fun::fun::Scalar;
use ecdsa_fun::nonce::Deterministic;
use ecdsa_fun::Signature;
use sha2::Sha256;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct TxRedeem {
    inner: Transaction,
    digest: Sighash,
    lock_output_descriptor: Descriptor<::bitcoin::PublicKey>,
    watch_script: ScriptBuf,
}

impl TxRedeem {
    pub fn new(tx_lock: &TxLock, redeem_address: &Address, spending_fee: Amount) -> Result<Self> {
        // lock_input is the shared output that is now being used as an input for the
        // redeem transaction
        let tx_redeem = tx_lock.build_spend_transaction(redeem_address, None, spending_fee)?;

        let digest = SighashCache::new(&tx_redeem)
            .p2wsh_signature_hash(
                0, // Only one input: lock_input (lock transaction)
                &tx_lock.output_descriptor.script_code().expect("scriptcode"),
                tx_lock.lock_amount(),
                EcdsaSighashType::All,
            )
            .expect("sighash").to_byte_array();

        Ok(Self {
            inner: tx_redeem,
            digest,
            lock_output_descriptor: tx_lock.output_descriptor.clone(),
            watch_script: redeem_address.script_pubkey(),
        })
    }

    pub fn txid(&self) -> Txid {
        self.inner.txid()
    }

    pub fn digest(&self) -> Sighash {
        self.digest
    }

    pub fn complete(
        mut self,
        encrypted_signature: EncryptedSignature,
        a: SecretKey,
        s_a: Scalar,
        B: PublicKey,
    ) -> Result<Transaction> {
        verify_encsig(
            B,
            PublicKey::from(s_a.clone()),
            &self.digest(),
            &encrypted_signature,
        )
        .context("Invalid encrypted signature received")?;

        let sig_a = a.sign(self.digest());
        let adaptor = Adaptor::<HashTranscript<Sha256>, Deterministic<Sha256>>::default();
        let sig_b = adaptor.decrypt_signature(&s_a, encrypted_signature);

        let satisfier = {
            let mut satisfier = HashMap::with_capacity(2);

            let A = ::bitcoin::PublicKey::from_slice(&a.public.to_bytes())?;
            let B = ::bitcoin::PublicKey::from_slice(&B.0.to_bytes())?;

            // The order in which these are inserted doesn't matter
            satisfier.insert(
                A,
                crate::bitcoin::to_ecdsa_signature(sig_a),
            );
            satisfier.insert(
                B,
                crate::bitcoin::to_ecdsa_signature(sig_b),
            );

            satisfier
        };

        self.lock_output_descriptor
            .satisfy(&mut self.inner.input[0], satisfier)
            .context("Failed to sign Bitcoin redeem transaction")?;

        Ok(self.inner)
    }

    pub fn extract_signature_by_key(
        &self,
        candidate_transaction: Transaction,
        B: PublicKey,
    ) -> Result<Signature> {
        let input = match candidate_transaction.input.as_slice() {
            [input] => input,
            [] => bail!(NoInputs),
            [inputs @ ..] => bail!(TooManyInputs(inputs.len())),
        };

        let sigs = match input.witness.iter().collect::<Vec<_>>().as_slice() {
            [sig_1, sig_2, _script] => [sig_1, sig_2]
                .iter()
                .map(|sig| ecdsa::Signature::from_der(&sig[..sig.len() - 1]).map(|sig| Signature::from_bytes(sig.serialize_compact()).expect("valid compact signature")))
                .collect::<std::result::Result<Vec<_>, _>>(),
            [] => bail!(EmptyWitnessStack),
            [witnesses @ ..] => bail!(NotThreeWitnesses(witnesses.len())),
        }?;

        let sig = sigs
            .into_iter()
            .find(|sig| verify_sig(&B, &self.digest(), sig).is_ok())
            .context("Neither signature on witness stack verifies against B")?;

        Ok(sig)
    }

    pub fn get_amount(&self) -> u64 {
        return self.inner.output.first().unwrap().value.to_sat();
    }

    pub fn weight() -> usize {
        548
    }

    pub fn vsize() -> usize {
        140
    }

    #[cfg(test)]
    pub fn inner(&self) -> Transaction {
        self.inner.clone()
    }
}

impl Watchable for TxRedeem {
    fn id(&self) -> Txid {
        self.txid()
    }

    fn script(&self) -> ScriptBuf {
        self.watch_script.clone()
    }
}
