use crate::bitcoin::wallet::{EstimateFeeRate, Watchable};
use crate::bitcoin::{
    build_shared_output_descriptor, Address, Amount, PublicKey, Transaction, Wallet,
};
use ::bitcoin::psbt::Psbt as PartiallySignedTransaction;
use ::bitcoin::{OutPoint, TxIn, TxOut, Txid, Witness};
use anyhow::{bail, Context, Result};
use miniscript::Descriptor;
use bitcoin::{absolute::LockTime, ScriptBuf, Sequence};
use serde::{Deserialize, Serialize};

const SCRIPT_SIZE: usize = 34;
const TX_LOCK_WEIGHT: usize = 485;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TxLock {
    inner: PartiallySignedTransaction,
    pub(in crate::bitcoin) output_descriptor: Descriptor<::bitcoin::PublicKey>,
}

impl TxLock {
    pub async fn new<C>(
        wallet: &Wallet<C>,
        amount: Amount,
        A: PublicKey,
        B: PublicKey,
        change: bitcoin::Address,
    ) -> Result<Self>
    where
        C: EstimateFeeRate + Send + Sync + 'static,
    {
        let lock_output_descriptor = build_shared_output_descriptor(A.0, B.0);
        let address = lock_output_descriptor
            .address(wallet.get_network())
            .expect("can derive address from descriptor");

        let fee = wallet
            .estimate_lock_fee(amount)
            .await
            .context("Failed to estimate lock transaction fee")?;
        let psbt = wallet
            .send_to_address_with_fee(
                address,
                amount,
                fee,
                Some(wallet.new_change_address().await.unwrap()),
            )
            .await?;

        Ok(Self {
            inner: psbt,
            output_descriptor: lock_output_descriptor,
        })
    }

    /// Creates an instance of `TxLock` from a PSBT, the public keys of the
    /// parties and the specified amount.
    ///
    /// This function validates that the given PSBT does indeed pay that
    /// specified amount to a shared output.
    pub fn from_psbt(
        psbt: PartiallySignedTransaction,
        A: PublicKey,
        B: PublicKey,
        btc: Amount,
    ) -> Result<Self> {
        if psbt.unsigned_tx.output.len() > 2 {
            bail!(
                "PSBT has {} outputs, expected at most two. Something is fishy!",
                psbt.unsigned_tx.output.len()
            );
        }

        let descriptor = build_shared_output_descriptor(A.0, B.0);
        let legit_shared_output_script = descriptor.script_pubkey();

        // The PSBT must pay the agreed amount to exactly one output of the shared
        // descriptor. Anything else (zero, or multiple shared-script outputs, or a
        // wrong amount) makes downstream spend/refund logic ambiguous.
        let shared_outputs = psbt
            .unsigned_tx
            .output
            .iter()
            .filter(|output| output.script_pubkey == legit_shared_output_script)
            .collect::<Vec<_>>();

        let shared_output = match shared_outputs.as_slice() {
            [shared_output] => shared_output,
            [..] => {
                bail!(
                    "PSBT must have exactly one output paying to the shared descriptor, found {}",
                    shared_outputs.len()
                );
            }
        };

        if shared_output.value != btc {
            bail!(
                "Shared output pays {} sats but the agreed amount is {} sats",
                shared_output.value,
                btc.to_sat()
            );
        }

        Ok(TxLock {
            inner: psbt,
            output_descriptor: descriptor,
        })
    }

    pub fn lock_amount(&self) -> Amount {
        self.inner.clone().extract_tx().expect("lock PSBT is fully signed").output[self.lock_output_vout()].value
    }

    pub fn txid(&self) -> Txid {
        self.inner.clone().extract_tx().expect("lock PSBT is fully signed").compute_txid()
    }

    pub fn as_outpoint(&self) -> OutPoint {
        // This is fine because a transaction that has that many outputs is not
        // realistic
        #[allow(clippy::cast_possible_truncation)]
        OutPoint::new(self.txid(), self.lock_output_vout() as u32)
    }

    /// Calculate the size of the script used by this transaction.
    pub fn script_size() -> usize {
        SCRIPT_SIZE
    }

    pub fn script_pubkey(&self) -> ScriptBuf {
        self.output_descriptor.script_pubkey()
    }

    /// Retreive the index of the locked output in the transaction outputs
    /// vector
    fn lock_output_vout(&self) -> usize {
        self.inner
            .clone()
            .extract_tx()
            .expect("lock PSBT is fully signed")
            .output
            .iter()
            .position(|output| output.script_pubkey == self.output_descriptor.script_pubkey())
            .expect("transaction contains lock output")
    }

    pub fn build_spend_transaction(
        &self,
        spend_address: &Address,
        sequence: Option<u32>,
        spending_fee: Amount,
    ) -> Result<Transaction> {
        let previous_output = self.as_outpoint();

        let sequence = Sequence(sequence.unwrap_or(0xFFFF_FFFF));
        let tx_in = TxIn {
            previous_output,
            script_sig: Default::default(),
            sequence,
            witness: Default::default(),
        };

        let fee = spending_fee;
        let lock_value = self.inner.clone().extract_tx().expect("lock PSBT is fully signed").output[self.lock_output_vout()].value;
        let value = lock_value
            .checked_sub(fee)
            .filter(|value| *value > Amount::ZERO)
            .with_context(|| {
                format!(
                    "Bitcoin spend fee {} sats must be less than lock output value {} sats",
                    fee, lock_value
                )
            })?;
        let tx_out = TxOut {
            value,
            script_pubkey: spend_address.script_pubkey(),
        };

        tracing::debug!(%fee, "Constructed Bitcoin spending transaction");

        Ok(Transaction {
            version: ::bitcoin::transaction::Version(2),
            lock_time: LockTime::ZERO,
            input: vec![tx_in],
            output: vec![tx_out],
        })
    }

    pub fn weight() -> usize {
        TX_LOCK_WEIGHT
    }

    pub fn vsize() -> usize {
        123
    }
}

impl From<TxLock> for PartiallySignedTransaction {
    fn from(from: TxLock) -> Self {
        from.inner
    }
}

impl Watchable for TxLock {
    fn id(&self) -> Txid {
        self.txid()
    }

    fn script(&self) -> ScriptBuf {
        self.output_descriptor.script_pubkey()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitcoin::wallet::StaticFeeRate;
    use crate::bitcoin::WalletBuilder;

    #[tokio::test]
    async fn given_bob_sends_good_psbt_when_reconstructing_then_succeeeds() {
        let (A, B) = alice_and_bob();
        let wallet = WalletBuilder::new(50_000).build();
        let agreed_amount = Amount::from_sat(10000);

        let psbt = bob_make_psbt(A, B, &wallet, agreed_amount).await;
        let result = TxLock::from_psbt(psbt, A, B, agreed_amount);

        result.expect("PSBT to be valid");
    }

    fn psbt_fee(psbt: &PartiallySignedTransaction) -> u64 {
        let input_value: Amount = psbt
            .inputs
            .iter()
            .map(|input| {
                input
                    .witness_utxo
                    .as_ref()
                    .expect("lock inputs must carry witness_utxo")
                    .value
            })
            .sum();
        let output_value: Amount = psbt
            .unsigned_tx
            .output
            .iter()
            .map(|output| output.value)
            .sum();
        (input_value - output_value).to_sat()
    }

    #[tokio::test]
    async fn lock_fee_uses_the_scaled_relay_floor() {
        let (A, B) = alice_and_bob();
        let wallet = WalletBuilder::new(50_000).with_fees(1.0, 1000).build();
        let amount = Amount::from_sat(10_000);

        let tx_lock = TxLock::new(
            &wallet,
            amount,
            A,
            B,
            wallet.new_change_address().await.unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(psbt_fee(&tx_lock.inner), 154);
    }

    #[tokio::test]
    async fn whole_deposit_lock_spends_the_input_without_a_change_output() {
        let (A, B) = alice_and_bob();
        let wallet = WalletBuilder::new(100_000).with_fees(1.0, 1000).build();
        let amount = Amount::from_sat(99_846);

        let tx_lock = TxLock::new(
            &wallet,
            amount,
            A,
            B,
            wallet.new_change_address().await.unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(tx_lock.inner.unsigned_tx.output.len(), 1);
        assert_eq!(tx_lock.inner.unsigned_tx.output[0].value, amount);
        assert_eq!(psbt_fee(&tx_lock.inner), 154);
    }

    #[tokio::test]
    async fn lock_fee_uses_the_relative_cap() {
        let (A, B) = alice_and_bob();
        let wallet = WalletBuilder::new(50_000).with_fees(100.0, 1000).build();
        let amount = Amount::from_sat(10_000);

        let tx_lock = TxLock::new(
            &wallet,
            amount,
            A,
            B,
            wallet.new_change_address().await.unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(psbt_fee(&tx_lock.inner), 2_000);
    }

    #[tokio::test]
    async fn bob_can_fund_without_a_change_output() {
        let (A, B) = alice_and_bob();
        let fees = 300;
        let agreed_amount = Amount::from_sat(10000);
        let amount = agreed_amount.to_sat() + fees;
        let wallet = WalletBuilder::new(amount).build();

        let psbt = bob_make_psbt(A, B, &wallet, agreed_amount).await;
        assert_eq!(
            psbt.unsigned_tx.output.len(),
            1,
            "psbt should only have a single output"
        );
        let result = TxLock::from_psbt(psbt, A, B, agreed_amount);

        result.expect("PSBT to be valid");
    }

    #[tokio::test]
    async fn given_bob_is_sending_less_than_agreed_when_reconstructing_txlock_then_fails() {
        let (A, B) = alice_and_bob();
        let wallet = WalletBuilder::new(50_000).build();
        let agreed_amount = Amount::from_sat(10000);

        let bad_amount = Amount::from_sat(5000);
        let psbt = bob_make_psbt(A, B, &wallet, bad_amount).await;
        let result = TxLock::from_psbt(psbt, A, B, agreed_amount);

        result.expect_err("PSBT to be invalid");
    }

    #[tokio::test]
    async fn given_bob_is_sending_to_a_bad_output_reconstructing_txlock_then_fails() {
        let (A, B) = alice_and_bob();
        let wallet = WalletBuilder::new(50_000).build();
        let agreed_amount = Amount::from_sat(10000);

        let E = eve();
        let psbt = bob_make_psbt(E, B, &wallet, agreed_amount).await;
        let result = TxLock::from_psbt(psbt, A, B, agreed_amount);

        result.expect_err("PSBT to be invalid");
    }

    #[test]
    fn given_psbt_with_two_shared_outputs_when_reconstructing_txlock_then_fails() {
        let (A, B) = alice_and_bob();
        let agreed_amount = Amount::from_sat(10000);

        let shared_script = build_shared_output_descriptor(A.0, B.0).script_pubkey();
        let tx = Transaction {
            version: ::bitcoin::transaction::Version(1),
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::new(),
                sequence: Sequence(0),
                witness: Witness::default(),
            }],
            output: vec![
                TxOut {
                    value: agreed_amount,
                    script_pubkey: shared_script.clone(),
                },
                TxOut {
                    value: agreed_amount,
                    script_pubkey: shared_script,
                },
            ],
        };
        let psbt = PartiallySignedTransaction::from_unsigned_tx(tx).unwrap();
        let result = TxLock::from_psbt(psbt, A, B, agreed_amount);

        result.expect_err("a PSBT with more than one shared-script output must be rejected");
    }

    proptest::proptest! {
        #[test]
        fn estimated_tx_lock_script_size_never_changes(a in crate::proptest::ecdsa_fun::point(), b in crate::proptest::ecdsa_fun::point()) {
            proptest::prop_assume!(a != b);

            let computed_size = build_shared_output_descriptor(a, b).script_pubkey().len();

            assert_eq!(computed_size, SCRIPT_SIZE);
        }
    }

    /// Helper function that represents Bob's action of constructing the PSBT.
    ///
    /// Extracting this allows us to keep the tests concise.
    async fn bob_make_psbt(
        A: PublicKey,
        B: PublicKey,
        wallet: &Wallet<StaticFeeRate>,
        amount: Amount,
    ) -> PartiallySignedTransaction {
        let change = wallet.new_change_address().await.unwrap();
        TxLock::new(wallet, amount, A, B, change)
            .await
            .unwrap()
            .into()
    }

    fn alice_and_bob() -> (PublicKey, PublicKey) {
        (PublicKey::random(), PublicKey::random())
    }

    fn eve() -> PublicKey {
        PublicKey::random()
    }
}
