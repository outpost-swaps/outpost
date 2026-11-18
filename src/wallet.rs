use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use uuid::Uuid;

use swap::env;
use swap::env::{Config};
use swap::seed::Seed;
use url::Url;

pub(crate) const BITCOIN_TARGET_BLOCK: usize = 2;

/// Use the same fresh per-swap wallet as the legacy swap flow.
///
/// Reusing a network-level BDK database is unsafe here: if that database was
/// created from a different seed, `Wallet::new` opens the stale descriptor and
/// derives a deposit address the wallet itself cannot see. That address is
/// displayed to the user, the deposit is broadcast, and the swap waits forever
/// for a deposit the syncing wallet will never register.
pub(crate) fn per_swap_wallet_dir(data_dir: &Path, swap_uuid: &Uuid) -> PathBuf {
    data_dir
        .join("tmp")
        .join(format!(".tmp_deposit_{swap_uuid}"))
}

/// Reuse one BDK database per seed. The old network-level database could have
/// been created from a different seed and display an address it could not see;
/// keying by the seed fingerprint keeps that failure impossible while avoiding
/// a full initial scan on every swap.
pub(crate) fn deposit_wallet_dir(data_dir: &Path, seed_fingerprint: &str) -> PathBuf {
    data_dir
        .join("bitcoin-deposit-v5")
        .join(seed_fingerprint)
}

/// Resume must keep using the wallet that funded the existing lock. New swaps
/// use a per-swap wallet; older swaps may have used the shared seed-keyed one.
/// Never fall back to the old network-level database: it can belong to a
/// different seed.
pub(crate) fn resume_deposit_wallet_dir(
    data_dir: &Path,
    swap_uuid: &Uuid,
    seed_fingerprint: &str,
) -> PathBuf {
    let per_swap = per_swap_wallet_dir(data_dir, swap_uuid);
    if per_swap.exists() {
        return per_swap;
    }
    deposit_wallet_dir(data_dir, seed_fingerprint)
}

/// The Electrum endpoints a swap wallet should use: the primary first, then any
/// extra pool members. Only `tcp://` endpoints are kept (the wallet rejects
/// direct `ssl://`, and one bad entry must not fail wallet creation); invalid
/// and duplicate entries are dropped.
pub(crate) fn electrum_endpoints(primary: &Url, extra: &[String]) -> Vec<Url> {
    let mut endpoints = vec![primary.clone()];
    for candidate in extra {
        match Url::parse(candidate) {
            Ok(url) if url.scheme() == "tcp" && !endpoints.contains(&url) => endpoints.push(url),
            _ => {}
        }
    }
    endpoints
}

pub(crate) async fn init_bitcoin_wallet(
    electrum_rpc_url: Url,
    electrum_pool_urls: &[String],
    electrum_proxy_string: &str,
    seed: &Seed,
    data_dir: PathBuf,
    env_config: env::Config,
    swaps_receive_index: u32,
    swaps_change_index: u32,
    dojo_url: Option<String>,
    dojo_api_key: Option<String>,
) -> Result<swap::bitcoin::Wallet> {
    let xprivkey = seed.derive_bip32_key(env_config.bitcoin_network)?;
    let wallet_started = Instant::now();
    tracing::info!(
        wallet_dir = %data_dir.display(),
        "Opening Bitcoin deposit wallet"
    );
    let electrum_endpoints = electrum_endpoints(&electrum_rpc_url, electrum_pool_urls);
    tracing::info!(
        endpoints = electrum_endpoints.len(),
        "Electrum endpoints for the swap wallet"
    );
    let wallet = swap::bitcoin::Wallet::new_with_urls(
        electrum_endpoints,
        electrum_proxy_string,
        &data_dir,
        xprivkey,
        env_config,
        BITCOIN_TARGET_BLOCK,
    )
        .await?;
    tracing::info!(
        elapsed_ms = wallet_started.elapsed().as_millis(),
        "Bitcoin deposit wallet opened"
    );

    let dojo_url = dojo_url.filter(|value| !value.trim().is_empty());
    let dojo_api_key = dojo_api_key.filter(|value| !value.trim().is_empty());
    match (dojo_url, dojo_api_key) {
        (Some(url), Some(api_key)) => {
            let url = Url::parse(&url).context("Invalid Dojo URL for the fee estimator")?;
            let tor_port = electrum_proxy_string
                .rsplit(':')
                .next()
                .and_then(|port| port.parse::<u16>().ok())
                .unwrap_or(0);
            wallet
                .require_dojo_fee_estimator(url, api_key, tor_port)
                .await?;
        }
        (None, None) => {
            wallet.require_dojo_fee_source();
            tracing::warn!(
                "No Dojo fee credentials supplied; wallet is recovery-only and will fail closed if a fresh fee estimate is needed"
            );
        }
        _ => bail!("Incomplete Dojo fee estimator credentials: URL and API key must both be set"),
    }

    let cache_started = Instant::now();
    wallet
        .seed_address_cache(swaps_receive_index, swaps_change_index)
        .await
        .context("Failed to seed bitcoin wallet address cache")?;
    tracing::info!(
        elapsed_ms = cache_started.elapsed().as_millis(),
        swaps_receive_index,
        swaps_change_index,
        "Bitcoin wallet address cache seeded"
    );

    // Seed from the paired Dojo first: it is the same indexer the GUI wallet
    // uses, so the balance shown to the user and the balance the swap can spend
    // come from one source. Electrum still supplies tip/mempool state below.
    let dojo_started = Instant::now();
    match wallet.seed_from_dojo().await {
        Ok(seeded) => tracing::info!(
            elapsed_ms = dojo_started.elapsed().as_millis(),
            seeded,
            "Dojo unspent snapshot applied"
        ),
        Err(error) => tracing::warn!(
            ?error,
            "Dojo unspent snapshot failed; continuing with the Electrum sync"
        ),
    }

    let sync_started = Instant::now();
    tracing::info!("Syncing Bitcoin deposit wallet");
    // Electrum sync is the one step that dies on a single flaky server or Tor
    // hiccup; a resume failure here parks the swap until the user clicks
    // Resume again. Retry a few times before surfacing the error.
    let mut sync_error = None;
    for attempt in 1..=3u32 {
        match wallet.sync().await {
            Ok(()) => {
                sync_error = None;
                break;
            }
            Err(error) => {
                tracing::warn!(
                    attempt,
                    elapsed_ms = sync_started.elapsed().as_millis(),
                    "Bitcoin wallet sync failed: {error:#}"
                );
                sync_error = Some(error);
                if attempt < 3 {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                }
            }
        }
    }
    if let Some(error) = sync_error {
        return Err(error).context("Failed to sync bitcoin wallet");
    }
    tracing::info!(
        elapsed_ms = sync_started.elapsed().as_millis(),
        "Bitcoin deposit wallet synced"
    );

    Ok(wallet)
}

pub(crate) async fn init_monero_wallet(
    monero_rpc_endpoint: String,
    env_config: Config,
    wallet_name: &str
) -> Result<swap::monero::Wallet> {
    let monero_wallet = swap::monero::Wallet::open_or_create(
        Url::from_str(monero_rpc_endpoint.as_str())
            .context("Invalid Monero RPC endpoint")?,
        wallet_name.to_string(),
        env_config,
    )
        .await?;
    Ok(monero_wallet)
}

/// Open the v5 Bob monitoring wallet without scanning from genesis.
///
/// v5 verifies/finalizes the shared lock with `check_tx_key`, which needs the
/// swap keys but not wallet ownership. A normal monitoring wallet scans from
/// height 0 and blocks the single wallet-RPC, so v5 uses a future-height
/// placeholder wallet instead. Legacy v1 paths keep `init_monero_wallet`
/// because they still use wallet-owned transfers.
pub(crate) async fn init_monero_check_wallet(
    monero_rpc_endpoint: String,
    env_config: Config,
    wallet_name: &str,
    restore_height: Option<u32>,
) -> Result<swap::monero::Wallet> {
    let url = Url::from_str(monero_rpc_endpoint.as_str())
        .context("Invalid Monero RPC endpoint")?;

    // Prefer the already-synced monitoring wallet for existing swaps. It was
    // created at height 0 and is only a few blocks behind; opening it is fast.
    // The check-only wallet is a fallback for swaps that do not have one.
    match swap::monero::Wallet::open_existing(url.clone(), wallet_name.to_string(), env_config.clone()).await {
        Ok(wallet) => return Ok(wallet),
        Err(error) => tracing::debug!(%error, "No existing monitoring wallet; creating check-only wallet"),
    }

    // A future restore height is not reliable: monero-wallet-rpc can clamp it
    // and start an auto-refresh from genesis, blocking every wallet RPC call.
    // Use a conservative recent height so a new check-only wallet only scans
    // the recent chain. Existing swaps use the fast path above.
    // Prefer the live chain tip supplied by the caller; the constants below are
    // a last-resort fallback and must never become the height recorded for a
    // swap's redeem wallet (it pins it thousands of blocks in the past).
    let restore_height = restore_height.unwrap_or(match env_config.monero_network {
        monero::Network::Mainnet => 3_600_000,
        monero::Network::Stagenet => 2_240_000,
        monero::Network::Testnet => 3_900_000,
    });
    let name = format!("{wallet_name}-checkonly-v2");
    let monero_wallet = swap::monero::Wallet::open_or_create_at_height(
        url,
        name,
        env_config,
        restore_height,
    )
        .await?;
    Ok(monero_wallet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const ONION_A: &str = "tcp://xufljktdbo542e6jyc7hat6ybnxx6km57rhmw5qpbzyducdcofyx2kqd.onion:50001";
    const ONION_B: &str = "tcp://k3tauelvqb724if4roauzo5v4u4ha74clouplx55o3xownsa5beigqad.onion:50001";

    // Taker pool (H7): the swap wallet used a single Electrum server and had
    // nothing to fail over to when it stalled (swaps f7a162f7, bf5f78fa).
    #[test]
    fn electrum_endpoints_puts_the_primary_first_and_adds_the_pool() {
        let primary = Url::parse(ONION_A).unwrap();
        let endpoints = electrum_endpoints(&primary, &[ONION_B.to_string()]);
        assert_eq!(
            endpoints,
            vec![Url::parse(ONION_A).unwrap(), Url::parse(ONION_B).unwrap()]
        );
    }

    #[test]
    fn electrum_endpoints_without_a_pool_is_just_the_primary() {
        let primary = Url::parse(ONION_A).unwrap();
        assert_eq!(electrum_endpoints(&primary, &[]), vec![primary]);
    }

    #[test]
    fn electrum_endpoints_drops_duplicates_invalid_and_non_tcp_entries() {
        let primary = Url::parse(ONION_A).unwrap();
        let pool = vec![
            ONION_A.to_string(),                 // duplicate of the primary
            ONION_B.to_string(),
            ONION_B.to_string(),                 // duplicate in the pool
            "ssl://blockstream.info:700".into(), // direct TLS is rejected by the wallet
            "not a url".into(),
            "".into(),
        ];
        assert_eq!(
            electrum_endpoints(&primary, &pool),
            vec![Url::parse(ONION_A).unwrap(), Url::parse(ONION_B).unwrap()]
        );
    }

    fn test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("outpost-wallet-dir-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn same_seed_reuses_the_same_deposit_wallet() {
        let data_dir = test_dir();
        let seed = "seed-a";

        assert_eq!(
            deposit_wallet_dir(&data_dir, seed),
            deposit_wallet_dir(&data_dir, seed)
        );
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn different_seeds_get_distinct_deposit_wallets() {
        let data_dir = test_dir();

        assert_ne!(
            deposit_wallet_dir(&data_dir, "seed-a"),
            deposit_wallet_dir(&data_dir, "seed-b")
        );
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn resume_uses_per_swap_wallet_when_it_exists() {
        let data_dir = test_dir();
        let swap_uuid = Uuid::new_v4();
        let per_swap = per_swap_wallet_dir(&data_dir, &swap_uuid);
        fs::create_dir_all(&per_swap).unwrap();

        assert_eq!(
            resume_deposit_wallet_dir(&data_dir, &swap_uuid, "seed-a"),
            per_swap
        );
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn resume_uses_seed_wallet_without_a_per_swap_wallet() {
        let data_dir = test_dir();
        let seeded = deposit_wallet_dir(&data_dir, "seed-a");

        assert_eq!(
            resume_deposit_wallet_dir(&data_dir, &Uuid::new_v4(), "seed-a"),
            seeded
        );
        let _ = fs::remove_dir_all(data_dir);
    }
}
