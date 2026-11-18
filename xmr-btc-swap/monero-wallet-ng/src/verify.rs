use zeroize::Zeroizing;

use monero_oxide_wallet::ed25519::{Point, Scalar};
use monero_oxide_wallet::interface::{ProvidesTransactions, TransactionsError};
use monero_oxide_wallet::transaction::{Pruned, Transaction};
use monero_oxide_wallet::{Scanner, ViewPair};

use crate::util::create_scannable_block_for_tx;

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("Transaction error: {0}")]
    Transaction(#[from] TransactionsError),
    #[error("Scan error: {0}")]
    Scan(#[from] monero_oxide_wallet::ScanError),
    #[error("Failed to create view pair: {0}")]
    ViewPair(#[from] monero_oxide_wallet::ViewPairError),
}

/// Verify that a transaction sends the expected amount to the given view pair.
///
/// This function fetches the transaction from the RPC, scans it using the view pair,
/// and returns `true` if the transaction contains any single output to this view pair with the expected amount.
///
/// # Arguments
/// * `provider` - A provider implementing the `ProvidesTransactions` trait
/// * `tx_id` - The transaction ID (hash)
/// * `public_spend_key` - The public spend key of the receiving wallet
/// * `private_view_key` - The private view key of the receiving wallet
/// * `expected_amount` - The expected amount in piconero
///
/// # Returns
/// * `Ok(true)` if the transaction contains any single output to this view pair with the expected amount
/// * `Ok(false)` if the amounts don't match or no outputs were found
/// * `Err(...)` if there was an error fetching or scanning the transaction
///
/// Note: This doesn't register any subaddresses which means it will only detect outputs that are sent to the primary address of the wallet.
pub async fn verify_transfer<P: ProvidesTransactions>(
    provider: &P,
    tx_id: [u8; 32],
    public_spend_key: Point,
    private_view_key: Zeroizing<Scalar>,
    expected_amount: u64,
) -> Result<bool, VerifyError> {
    // Fetch the transaction
    let tx: Transaction<Pruned> = provider.pruned_transaction(tx_id).await?;

    // Create the view pair
    let view_pair = ViewPair::new(public_spend_key, private_view_key)?;

    // Create a scanner
    let mut scanner = Scanner::new(view_pair);

    // Create a fake ScannableBlock containing with just this transaction.
    // The output_index_for_first_ringct_output is garbage (0) but we don't care
    // since we're only verifying amounts, not spending.
    let scannable_block = create_scannable_block_for_tx(vec![(tx_id, tx)]);

    // Scan the block
    let outputs = scanner.scan(scannable_block)?;

    // Ignore any timelocked outputs to protect against unspendable outputs
    let outputs = outputs.not_additionally_locked();

    // Check if any of the outputs have the expected amount
    let has_expected_amount_output = outputs
        .iter()
        .any(|output| output.commitment().amount == expected_amount);

    Ok(has_expected_amount_output)
}

/// The amount of the largest output the given view pair receives in a
/// transaction, or `None` if it receives no outputs. This mirrors what a sweep
/// of the transaction can spend, which always picks the single largest output.
pub async fn largest_received_utxo<P: ProvidesTransactions>(
    provider: &P,
    tx_id: [u8; 32],
    public_spend_key: Point,
    private_view_key: Zeroizing<Scalar>,
) -> Result<Option<u64>, VerifyError> {
    let tx: Transaction<Pruned> = provider.pruned_transaction(tx_id).await?;

    let view_pair = ViewPair::new(public_spend_key, private_view_key)?;
    let mut scanner = Scanner::new(view_pair);

    let scannable_block = create_scannable_block_for_tx(vec![(tx_id, tx)]);
    let outputs = scanner.scan(scannable_block)?.not_additionally_locked();

    Ok(outputs
        .iter()
        .map(|output| output.commitment().amount)
        .max())
}

/// Whether `tx_id` pays the view pair at least `minimum_amount` in a single
/// output that can be spent now.
///
/// Wallet-RPC `check_tx_key` sums every output to the address and ignores the
/// transaction's unlock time, so on its own it accepts a lock that pays the
/// right amount but cannot be spent for years, or one split across several
/// small outputs. The sweep spends only the single largest unlocked output, so
/// this checks exactly that.
pub async fn has_spendable_output<P: ProvidesTransactions>(
    provider: &P,
    tx_id: [u8; 32],
    public_spend_key: Point,
    private_view_key: Zeroizing<Scalar>,
    minimum_amount: u64,
) -> Result<bool, VerifyError> {
    let largest = largest_received_utxo(provider, tx_id, public_spend_key, private_view_key).await?;
    Ok(largest.is_some_and(|amount| amount >= minimum_amount))
}

#[cfg(test)]
mod tests {
    use std::future::Future;

    use monero_oxide_wallet::interface::InterfaceError;
    use monero_oxide_wallet::transaction::Timelock;

    use super::*;
    use crate::empty::tests::{received_transaction, view_pair_keys};

    /// Serves one fixed transaction for every request.
    struct OneTransaction(Transaction<Pruned>);

    impl ProvidesTransactions for OneTransaction {
        fn transactions(
            &self,
            _hashes: &[[u8; 32]],
        ) -> impl Send + Future<Output = Result<Vec<Transaction>, TransactionsError>> {
            async { Err(InterfaceError::InternalError("unused".to_owned()).into()) }
        }

        fn pruned_transactions(
            &self,
            hashes: &[[u8; 32]],
        ) -> impl Send + Future<Output = Result<Vec<Transaction<Pruned>>, TransactionsError>> {
            let transaction = self.0.clone();
            let count = hashes.len();
            async move { Ok(vec![transaction; count]) }
        }

        fn transaction(
            &self,
            _hash: [u8; 32],
        ) -> impl Send + Future<Output = Result<Transaction, TransactionsError>> {
            async { Err(InterfaceError::InternalError("unused".to_owned()).into()) }
        }

        fn pruned_transaction(
            &self,
            _hash: [u8; 32],
        ) -> impl Send + Future<Output = Result<Transaction<Pruned>, TransactionsError>> {
            let transaction = self.0.clone();
            async move { Ok(transaction) }
        }
    }

    async fn received_amount(provider: &OneTransaction) -> Option<u64> {
        let (public_spend_key, private_view_key) = view_pair_keys();
        largest_received_utxo(provider, [1; 32], public_spend_key, private_view_key)
            .await
            .unwrap()
    }

    async fn spendable(provider: &OneTransaction, minimum: u64) -> bool {
        let (public_spend_key, private_view_key) = view_pair_keys();
        has_spendable_output(provider, [1; 32], public_spend_key, private_view_key, minimum)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_unlocked_output_of_the_expected_amount_is_spendable() {
        let provider = OneTransaction(received_transaction().tx);
        let amount = received_amount(&provider)
            .await
            .expect("the fixture pays the view pair");

        assert!(spendable(&provider, amount).await);
        assert!(spendable(&provider, 1).await);
    }

    #[tokio::test]
    async fn an_output_below_the_expected_amount_is_not_spendable() {
        let provider = OneTransaction(received_transaction().tx);
        let amount = received_amount(&provider).await.unwrap();

        assert!(!spendable(&provider, amount + 1).await);
    }

    // A malicious maker can lock the right amount with a far-future unlock time.
    // `check_tx_key` reports it as received, but the sweep cannot spend it, so
    // Bob would send his signature and end up with neither BTC nor XMR.
    #[tokio::test]
    async fn a_timelocked_output_is_not_spendable_even_for_the_right_amount() {
        let mut transaction = received_transaction().tx;
        let unlocked = OneTransaction(transaction.clone());
        let amount = received_amount(&unlocked).await.unwrap();

        transaction.prefix_mut().additional_timelock = Timelock::Block(usize::MAX);
        let locked = OneTransaction(transaction);

        assert_eq!(received_amount(&locked).await, None);
        assert!(!spendable(&locked, amount).await);
        assert!(!spendable(&locked, 1).await);
    }

    #[tokio::test]
    async fn a_transaction_that_does_not_pay_the_view_pair_is_not_spendable() {
        let provider = OneTransaction(received_transaction().tx);
        // A different view key cannot see the output.
        let (public_spend_key, _) = view_pair_keys();
        let other_view_key = Zeroizing::new(Scalar::read(&mut [3u8; 32].as_slice()).unwrap());

        let result = has_spendable_output(&provider, [1; 32], public_spend_key, other_view_key, 1)
            .await
            .unwrap();

        assert!(!result);
    }
}
