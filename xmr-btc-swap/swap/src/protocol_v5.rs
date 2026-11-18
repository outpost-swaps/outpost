//! Protocol 5.0 engine seam.
//!
//! The Eigenwallet-derived state machine and p2p stack are linked only when
//! the `protocol-v5` feature is enabled. The taker path is live; the maker
//! event loop and Alice state driver are in place but the ASB binary still
//! uses the legacy protocol until the Phase 3 cutover is complete.

pub use bitcoin_wallet;
pub use protocol_swap_core as core;
pub mod alice;
pub mod bob;
pub mod event_loop;
pub mod recovery;
pub mod store;
pub use protocol_swap_db as db;
pub use protocol_swap_env as env;
pub use protocol_swap_feed as feed;
pub use protocol_swap_machine as machine;
pub use protocol_swap_p2p as p2p;
pub use protocol_utxo_ownership_proof as ownership_proof;

/// Convert Outpost's app environment into the 5.0 engine configuration.
///
/// Timeouts, finality, and networks are local policy and may follow Outpost.
/// The cancel/punish timelocks are part of the 5.0 protocol and must stay at
/// Eigenwallet 5.0.2's values: importing the legacy 1.0 timelocks here made
/// testnet v5 swaps use a 12-block cancel window instead of 42.
pub fn protocol_config(config: crate::env::Config) -> env::env::Config {
    use env::env::{GetConfig, Mainnet, Regtest, Testnet};

    let mut converted = match config.bitcoin_network {
        bitcoin::Network::Bitcoin => Mainnet::get_config(),
        bitcoin::Network::Testnet | bitcoin::Network::Testnet4 => Testnet::get_config(),
        bitcoin::Network::Regtest | bitcoin::Network::Signet => Regtest::get_config(),
    };

    // Outpost's app env is authoritative for these bounds: the values copied
    // here override the engine defaults (testnet lock-confirmed is 12 h in
    // `crate::env`, not the engine's own default).
    converted.bitcoin_lock_mempool_timeout = config.bitcoin_lock_mempool_timeout;
    converted.bitcoin_lock_confirmed_timeout = config.bitcoin_lock_confirmed_timeout;
    converted.bitcoin_finality_confirmations = config.bitcoin_finality_confirmations;
    converted.bitcoin_avg_block_time = config.bitcoin_avg_block_time;
    converted.bitcoin_network = config.bitcoin_network;
    converted.monero_avg_block_time = config.monero_avg_block_time;
    converted.monero_finality_confirmations = config.monero_finality_confirmations;
    converted.monero_network = match config.monero_network {
        monero::Network::Mainnet => monero_oxide_wallet::address::Network::Mainnet,
        monero::Network::Testnet => monero_oxide_wallet::address::Network::Testnet,
        monero::Network::Stagenet => monero_oxide_wallet::address::Network::Stagenet,
    };

    converted
}

/// Broadcast Bob's signed Bitcoin lock and return its txid.
///
/// This is the explicit broadcast step after the 5.0 setup commit; construction
/// itself never relays.
pub async fn broadcast_lock(
    wallet: &crate::bitcoin::Wallet,
    state3: &machine::bob::State3,
) -> anyhow::Result<bitcoin::Txid> {
    let tx = state3
        .tx_lock
        .as_psbt()
        .clone()
        .extract_tx_unchecked_fee_rate();
    let (txid, _subscription) =
        bitcoin_wallet::BitcoinWallet::ensure_broadcasted(&**wallet, tx, "lock").await?;
    Ok(txid)
}

/// Borrow Outpost's wallet as the trait object the 5.0 engine expects.
pub fn as_protocol_wallet<C>(
    wallet: &crate::bitcoin::Wallet<C>,
) -> &dyn bitcoin_wallet::BitcoinWallet
where
    bitcoin_wallet::Wallet<bitcoin_wallet::Connection, C>: bitcoin_wallet::BitcoinWallet,
{
    &**wallet
}

#[cfg(test)]
mod config_tests {
    use super::*;
    use crate::env::GetConfig as _;

    #[test]
    fn v5_config_keeps_eigen_timelocks() {
        let testnet = protocol_config(crate::env::Testnet::get_config());
        // Protocol compatibility: Eigenwallet 5.0.2 uses a 42-block testnet
        // cancel window. Do not import the legacy 12 here.
        assert_eq!(testnet.bitcoin_cancel_timelock, 42);
        assert_eq!(testnet.bitcoin_punish_timelock, 72);
        // The app env overrides the engine default; testnet4 must not
        // early-refund a healthy lock after 1 h.
        assert_eq!(
            testnet.bitcoin_lock_confirmed_timeout,
            std::time::Duration::from_secs(12 * 60 * 60)
        );
        assert_eq!(testnet.bitcoin_finality_confirmations, 2);

        let mainnet = protocol_config(crate::env::Mainnet::get_config());
        assert_eq!(mainnet.bitcoin_cancel_timelock, 30);
        assert_eq!(mainnet.bitcoin_punish_timelock, 144);
        assert_eq!(
            mainnet.bitcoin_lock_confirmed_timeout,
            std::time::Duration::from_secs(2 * 60 * 60)
        );
        assert_eq!(mainnet.bitcoin_finality_confirmations, 1);
    }
}

#[cfg(test)]
mod tests;
