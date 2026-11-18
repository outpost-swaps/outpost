#![warn(
    unused_extern_crates,
    missing_copy_implementations,
    rust_2018_idioms,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::fallible_impl_from,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::dbg_macro
)]
#![forbid(unsafe_code)]
#![allow(non_snake_case)]

use anyhow::{bail, Context, Result};
use comfy_table::Table;
use libp2p::core::multiaddr::Protocol;
use libp2p::core::Multiaddr;
use libp2p::Swarm;
use rust_decimal::prelude::ToPrimitive;
use std::convert::TryInto;
use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::SystemTime;
use structopt::clap;
use structopt::clap::ErrorKind;
use swap::asb::command::{parse_args, Arguments, Command};
use swap::asb::config::{
    initial_setup, query_user_for_initial_config, read_config, Config, ConfigNotInitialized,
};
use swap::asb::liquidity::Reservations;
use swap::asb::{
    cancel, punish, redeem, refund, safely_abort, EventLoop, Finality, FixedRate, KrakenRate,
    RateSource,
};
use swap::common::check_latest_version;
use swap::database::{open_db_with_cipher, StateCipher};
use swap::network::rendezvous::XmrBtcNamespace;
use swap::network::swarm;
use swap::protocol::alice::{run, AliceState};
use swap::seed::Seed;
use swap::tor::AuthenticatedClient;
use swap::{asb, bitcoin, kraken, monero, tor, util};
use tracing_subscriber::filter::LevelFilter;
use url::Url;

const DEFAULT_WALLET_NAME: &str = "asb-wallet";

#[tokio::main]
async fn main() -> Result<()> {
    util::install_default_crypto_provider();
    let Arguments {
        testnet,
        json,
        disable_timestamp,
        config_path,
        env_config,
        cmd,
    } = match parse_args(env::args_os()) {
        Ok(args) => args,
        Err(e) => {
            if let Some(clap_err) = e.downcast_ref::<clap::Error>() {
                match clap_err.kind {
                    ErrorKind::HelpDisplayed | ErrorKind::VersionDisplayed => {
                        println!("{}", clap_err.message);
                        std::process::exit(0);
                    }
                    _ => {
                        bail!(e);
                    }
                }
            }
            bail!(e);
        }
    };

    asb::tracing::init(LevelFilter::DEBUG, json, !disable_timestamp).expect("initialize tracing");

    let config = match read_config(config_path.clone())? {
        Ok(config) => config,
        Err(ConfigNotInitialized {}) => {
            initial_setup(config_path.clone(), query_user_for_initial_config(testnet)?)?;
            read_config(config_path)?.expect("after initial setup config can be read")
        }
    };

    if config.monero.network != env_config.monero_network {
        bail!(format!(
            "Expected monero network in config file to be {:?} but was {:?}",
            env_config.monero_network, config.monero.network
        ));
    }
    if config.bitcoin.network != env_config.bitcoin_network {
        bail!(format!(
            "Expected bitcoin network in config file to be {:?} but was {:?}",
            env_config.bitcoin_network, config.bitcoin.network
        ));
    }

    let seed_stdin = matches!(
        cmd,
        Command::Start {
            seed_stdin: true,
            ..
        }
    );
    let seed = if seed_stdin {
        // The seed, and the Dojo API key when the GUI sends one, come over stdin rather than
        // the environment (audit M-6).
        let mut payload = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut payload)
            .context("Could not read ASB seed from stdin")?;
        let secrets = swap::asb::stdin_secrets::parse(&payload);
        swap::asb::stdin_secrets::remember_dojo_api_key(secrets.dojo_api_key);
        Seed::from_base64(&secrets.seed_base64).context("Could not read ASB seed from stdin")?
    } else {
        if matches!(cmd, Command::Start { .. }) {
            tracing::warn!("ASB is using the standalone persistent seed file; GUI launches should use --seed-stdin");
        }
        Seed::from_file_or_generate(&config.data.dir)
            .context("Could not retrieve/initialize seed")?
    };

    let db = open_db_with_cipher(
        config.data.dir.join("sqlite"),
        Some(StateCipher::from_seed(&seed)),
    )
    .await?;

    let direct_regtest = env_config.bitcoin_network == bitcoin::Network::Regtest;
    let tor_port = if direct_regtest {
        0
    } else {
        config.tor.socks5_port
    };
    if tor_port == 0 && !direct_regtest {
        bail!("Tor is required: refusing to run the ASB without a SOCKS5 proxy");
    }
    let proxy_string = if direct_regtest {
        String::new()
    } else {
        format!("127.0.0.1:{}", tor_port)
    };
    tracing::info!(%proxy_string, direct_regtest, "Using SOCKS5 proxy at");

    if !direct_regtest {
        if let Err(e) = check_latest_version(env!("CARGO_PKG_VERSION"), tor_port).await {
            eprintln!("{}", e);
        }
    }

    match cmd {
        Command::Start { resume_only, .. } => {
            // check and warn for duplicate rendezvous points
            let mut rendezvous_addrs = config.network.rendezvous_point.clone();
            let prev_len = rendezvous_addrs.len();
            rendezvous_addrs.sort();
            rendezvous_addrs.dedup();
            let new_len = rendezvous_addrs.len();
            if new_len < prev_len {
                tracing::warn!(
                    "`rendezvous_point` config has {} duplicate entries, they are being ignored.",
                    prev_len - new_len
                );
            }

            let monero_wallet = match init_monero_wallet(&config, env_config).await {
                Ok(wallet) => wallet,
                Err(error) => {
                    // reported, not a panic: the GUI shows the cause and restarts the ASB with backoff
                    tracing::info!(error = %format!("{:#}", error), "ASB_ERROR_INITIALIZING_XMR_WALLET");
                    return Err(error);
                }
            };
            let monero_address = monero_wallet.get_main_address();
            let monero_balance = monero_wallet
                .get_balance()
                .await
                .expect("Failed to get monero balance");

            let bitcoin_wallet =
                match init_bitcoin_wallet(&config, &seed, env_config, proxy_string.clone()).await {
                    Ok(wallet) => wallet,
                    Err(error) => {
                        // reported, not a panic: the GUI shows the cause and restarts with backoff
                        tracing::info!(error = %format!("{error:#}"), "ASB_ERROR_INITIALIZING_BTC_WALLET");
                        return Err(error);
                    }
                };
            let bitcoin_balance = bitcoin_wallet
                .balance()
                .await
                .expect("Failed to get bitcoin balance")
                .to_sat();

            let latest_rate = match config.maker.manual_price {
                Some(manual_price) => {
                    let price_btc = manual_price
                        .to_f64()
                        .context("Manual price does not fit into f64")?;
                    tracing::info!(
                        manual_price = %manual_price,
                        "ASB_USING_MANUAL_PRICE"
                    );
                    RateSource::Fixed(FixedRate::from_price_btc(price_btc)?)
                }
                None => {
                    let price_updates = kraken::connect_with_fallback(
                        Url::parse(kraken::MONEROPRICE_NOW_URL)
                            .context("Failed to parse primary price source URL")?,
                        config.maker.price_ticker_ws_url.clone(),
                        tor_port,
                    )?;
                    // the normal ASB from COMIT uses Kraken for price data; this uses moneropricenow as primary, feather (which has a .onion for Tor usage) as websocket fallback, and Kraken's REST ticker as a last resort
                    RateSource::Market(KrakenRate::new(config.maker.ask_spread, price_updates))
                }
            };
            #[cfg(feature = "protocol-v5")]
            {
                return swap::asb::v5_start::run(
                    config.clone(),
                    &seed,
                    env_config,
                    testnet,
                    resume_only,
                    Arc::new(bitcoin_wallet),
                    Arc::new(monero_wallet),
                    latest_rate.clone(),
                    proxy_string.clone(),
                )
                .await;
            }

            let namespace = XmrBtcNamespace::from_is_testnet(testnet);

            tracing::info!("ASB_SETTING_UP_LIBP2P_SWARM");
            let liquidity = std::sync::Arc::new(Reservations::new());
            let mut swarm = swarm::asb(
                &seed,
                config.maker.min_buy_btc,
                config.maker.max_buy_btc,
                latest_rate.clone(),
                resume_only,
                env_config,
                namespace,
                &rendezvous_addrs,
                tor_port,
                liquidity.clone(),
            )
            .await?;

            let asb_peer_id = &mut swarm.local_peer_id().clone().to_string();

            for listen in config.network.listen.clone() {
                if let Err(e) = Swarm::listen_on(&mut swarm, listen.clone()) {
                    tracing::warn!("Failed to listen on network interface {}: {}. Consider removing it from the config.", listen, e);
                }
            }

            tracing::info!(peer_id = %swarm.local_peer_id(), "Network layer initialized");

            let mut multiaddr = String::new();
            for external_address in config.network.external_addresses {
                if multiaddr.is_empty() {
                    multiaddr = external_address.clone().to_string();
                }
                tracing::info!(%external_address, "ASB_REGISTERING_ADDRESS_WITH_RENDEZVOUS");
                Swarm::add_external_address(&mut swarm, external_address);
            }

            let bitcoin_wallet = Arc::new(bitcoin_wallet);
            let monero_wallet = Arc::new(monero_wallet);
            let (event_loop, mut swap_receiver) = EventLoop::new(
                swarm,
                env_config,
                bitcoin_wallet.clone(),
                monero_wallet,
                db.clone(),
                latest_rate.clone(),
                config.maker.min_buy_btc,
                config.maker.max_buy_btc,
                config.maker.external_bitcoin_redeem_address,
                liquidity.clone(),
            )
            .unwrap();

            // Auto-recovery sweep: punish swaps that are stuck in a terminal
            // state while the buyer's BTC is only recoverable through the
            // punish path (e.g. aborted before the XMR lock, buyer cancelled,
            // punish timelock expired). punish() enforces the timelock
            // fail-closed, so refundable/unfunded swaps are never touched.
            {
                let db_auto = db.clone();
                let wallet_auto = bitcoin_wallet.clone();
                tokio::spawn(async move {
                    // Give the initial resume logic a minute to settle first.
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    loop {
                        if let Err(error) =
                            asb::auto_punish_stuck_swaps(db_auto.clone(), wallet_auto.clone()).await
                        {
                            tracing::warn!("ASB_AUTO_PUNISH_SWEEP_FAILED: {}", error);
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(15 * 60)).await;
                    }
                });
            }

            tokio::spawn(async move {
                while let Some(swap) = swap_receiver.recv().await {
                    let rate = latest_rate.clone();
                    let liquidity = liquidity.clone();
                    tokio::spawn(async move {
                        let swap_id = swap.swap_id;
                        let final_state = match run(swap, rate).await {
                            Ok(state) => {
                                tracing::info!(%state, %swap_id, "ASB_SWAP_COMPLETE");
                                Some(state)
                            }
                            Err(error) => {
                                tracing::info!(%error, %swap_id, "ASB_SWAP_FAIL");
                                None
                            }
                        };

                        // Release the swap's liquidity reservation only when
                        // the swap reached a terminal state; failed swaps
                        // stay resumable and keep their promise until they
                        // complete (the startup re-reservation re-arms them
                        // after a restart).
                        match final_state {
                            Some(state) if swap::protocol::alice::swap::is_complete(&state) => {
                                if let Some(amount) = liquidity.release(swap_id) {
                                    tracing::info!(
                                        %swap_id,
                                        %amount,
                                        state = %state,
                                        "ASB_LIQUIDITY_RELEASED"
                                    );
                                }
                            }
                            Some(_) => tracing::warn!(
                                %swap_id,
                                "Swap task ended before a terminal state; keeping its liquidity reservation"
                            ),
                            None => tracing::warn!(
                                %swap_id,
                                "Swap task failed; keeping its liquidity reservation until the swap completes"
                            ),
                        }
                    });
                }
            });

            tracing::info!(%asb_peer_id, %multiaddr, %monero_balance.balance, %monero_balance.unlocked_balance, %monero_address, %bitcoin_balance, "ASB_INITIALIZED");

            event_loop.run().await;
        }
        Command::History => {
            let mut table = Table::new();

            table.set_header(vec!["SWAP ID", "STATE"]);

            for (swap_id, state) in db.all().await? {
                let state: AliceState = state.try_into()?;
                table.add_row(vec![swap_id.to_string(), state.to_string()]);
            }

            #[cfg(feature = "protocol-v5")]
            {
                let v5_db = swap::database::SqliteDatabase::open_with_cipher(
                    config.data.dir.join("sqlite"),
                    Some(StateCipher::from_seed(&seed)),
                )
                .await?;
                for (swap_id, value, _) in v5_db.all_protocol_v5_states("alice").await? {
                    if let Ok(state) =
                        serde_json::from_value::<swap::protocol_v5::db::alice::Alice>(value)
                    {
                        let state: swap::protocol_v5::machine::alice::AliceState = state.into();
                        table.add_row(vec![swap_id.to_string(), state.to_string()]);
                    }
                }
            }

            println!("{}", table);
        }
        Command::Config => {
            let config_json = serde_json::to_string_pretty(&config)?;
            println!("{}", config_json);
        }
        Command::WithdrawBtc { amount, address } => {
            let bitcoin_wallet =
                init_bitcoin_wallet(&config, &seed, env_config, proxy_string).await;
            if bitcoin_wallet.is_err() {
                tracing::info!("ASB_ERROR_INITIALIZING_BTC_WALLET");
            }
            let bitcoin_wallet = bitcoin_wallet.unwrap();

            let amount = match amount {
                Some(amount) => amount,
                None => {
                    bitcoin_wallet
                        .max_giveable(address.script_pubkey().len())
                        .await?
                }
            };

            let psbt = bitcoin_wallet
                .send_to_address(address, amount, None)
                .await?;
            let signed_tx = bitcoin_wallet.sign_and_finalize(psbt).await?;

            bitcoin_wallet.broadcast(signed_tx, "withdraw").await?;
        }
        Command::Balance => {
            let monero_wallet = match init_monero_wallet(&config, env_config).await {
                Ok(wallet) => wallet,
                Err(error) => {
                    // reported, not a panic: the GUI shows the cause and restarts the ASB with backoff
                    tracing::info!(error = %format!("{:#}", error), "ASB_ERROR_INITIALIZING_XMR_WALLET");
                    return Err(error);
                }
            };
            let monero_balance = monero_wallet.get_balance().await?;
            tracing::info!(%monero_balance);

            let bitcoin_wallet =
                init_bitcoin_wallet(&config, &seed, env_config, proxy_string).await;
            if bitcoin_wallet.is_err() {
                tracing::info!("ASB_ERROR_INITIALIZING_BTC_WALLET");
            }
            let bitcoin_wallet = bitcoin_wallet.unwrap();
            let bitcoin_balance = bitcoin_wallet.balance().await?;
            tracing::info!(%bitcoin_balance);
            tracing::info!(%bitcoin_balance, %monero_balance, "Current balance");
        }
        Command::Cancel { swap_id } => {
            let bitcoin_wallet =
                init_bitcoin_wallet(&config, &seed, env_config, proxy_string).await;
            if bitcoin_wallet.is_err() {
                tracing::info!("ASB_ERROR_INITIALIZING_BTC_WALLET");
            }
            let bitcoin_wallet = bitcoin_wallet.unwrap();

            let (txid, _) = cancel(swap_id, Arc::new(bitcoin_wallet), db).await?;

            tracing::info!("Cancel transaction successfully published with id {}", txid);
            tracing::info!(%swap_id, %txid, "ASB_CANCEL_TX");
        }
        Command::Refund { swap_id } => {
            let bitcoin_wallet =
                init_bitcoin_wallet(&config, &seed, env_config, proxy_string).await;
            if bitcoin_wallet.is_err() {
                tracing::info!("ASB_ERROR_INITIALIZING_BTC_WALLET");
            }
            let bitcoin_wallet = bitcoin_wallet.unwrap();
            let monero_wallet = match init_monero_wallet(&config, env_config).await {
                Ok(wallet) => wallet,
                Err(error) => {
                    // reported, not a panic: the GUI shows the cause and restarts the ASB with backoff
                    tracing::info!(error = %format!("{:#}", error), "ASB_ERROR_INITIALIZING_XMR_WALLET");
                    return Err(error);
                }
            };

            refund(
                swap_id,
                Arc::new(bitcoin_wallet),
                Arc::new(monero_wallet),
                db,
            )
            .await?;

            tracing::info!("Monero successfully refunded");
            tracing::info!(%swap_id, "ASB_REFUND");
        }
        Command::Punish { swap_id } => {
            let bitcoin_wallet =
                init_bitcoin_wallet(&config, &seed, env_config, proxy_string).await;
            if bitcoin_wallet.is_err() {
                tracing::info!("ASB_ERROR_INITIALIZING_BTC_WALLET");
            }
            let bitcoin_wallet = bitcoin_wallet.unwrap();

            let (txid, _) = punish(swap_id, Arc::new(bitcoin_wallet), db).await?;

            match txid {
                Some(txid) => {
                    tracing::info!("Punish transaction successfully published with id {}", txid);
                    tracing::info!(%swap_id, %txid, "ASB_PUNISH_TX");
                }
                None => {
                    tracing::info!("Lock output was already spent; swap marked as punished");
                    tracing::info!(%swap_id, "ASB_PUNISH_ALREADY_SPENT");
                }
            }
        }
        Command::SafelyAbort { swap_id } => {
            safely_abort(swap_id, db).await?;

            tracing::info!("Swap safely aborted");
            tracing::info!(%swap_id, "ASB_SAFELY_ABORTED");
        }
        Command::Redeem {
            swap_id,
            do_not_await_finality,
        } => {
            let bitcoin_wallet =
                init_bitcoin_wallet(&config, &seed, env_config, proxy_string).await;
            if bitcoin_wallet.is_err() {
                tracing::info!("ASB_ERROR_INITIALIZING_BTC_WALLET");
            }
            let bitcoin_wallet = bitcoin_wallet.unwrap();

            let (txid, _) = redeem(
                swap_id,
                Arc::new(bitcoin_wallet),
                db,
                Finality::from_bool(do_not_await_finality),
            )
            .await?;

            tracing::info!("Redeem transaction successfully published with id {}", txid);
            tracing::info!(%swap_id, %txid, "ASB_REDEEM_TX");
        }
        Command::ExportBitcoinWallet => {
            let bitcoin_wallet =
                init_bitcoin_wallet(&config, &seed, env_config, proxy_string).await;
            if bitcoin_wallet.is_err() {
                tracing::info!("ASB_ERROR_INITIALIZING_BTC_WALLET");
            }
            let bitcoin_wallet = bitcoin_wallet.unwrap();
            let wallet_export = bitcoin_wallet.wallet_export("asb").await?;
            println!("{}", wallet_export.to_string())
        }
    }

    Ok(())
}

async fn configure_required_dojo_fee_estimator(
    wallet: &bitcoin::Wallet,
    proxy_string: &str,
) -> Result<()> {
    let dojo_url = env::var("OUTPOST_DOJO_URL").ok().filter(|value| !value.trim().is_empty());
    let dojo_api_key = swap::asb::stdin_secrets::choose_dojo_api_key(
        swap::asb::stdin_secrets::dojo_api_key(),
        env::var("OUTPOST_DOJO_API_KEY").ok(),
    );
    match (dojo_url, dojo_api_key) {
        (Some(url), Some(api_key)) => {
            let url = Url::parse(&url).context("Invalid OUTPOST_DOJO_URL")?;
            let tor_port = proxy_string
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
                "No OUTPOST_DOJO_URL/OUTPOST_DOJO_API_KEY supplied; ASB is recovery-only and will fail closed if a fresh fee estimate is needed"
            );
        }
        _ => bail!("OUTPOST_DOJO_URL and OUTPOST_DOJO_API_KEY must be set together"),
    }
    Ok(())
}

async fn init_bitcoin_wallet(
    config: &Config,
    seed: &Seed,
    env_config: swap::env::Config,
    proxy_string: String,
) -> Result<bitcoin::Wallet> {
    let data_dir = &config.data.dir;
    let wallet = within_electrum_connect_limit(
        bitcoin::Wallet::new_samourai_asb_with_urls(
            config.bitcoin.electrum_urls(),
            proxy_string.as_str(),
            data_dir,
            seed.derive_bip32_key(config.bitcoin.network)?,
            env_config,
            config.bitcoin.target_block,
        ),
        ELECTRUM_CONNECT_LIMIT,
    )
    .await;
    if wallet.is_ok() {
        let wallet = wallet.unwrap();
        configure_required_dojo_fee_estimator(&wallet, &proxy_string).await?;
        tracing::info!("ASB_INITIALIZED_BITCOIN_WALLET");
        tracing::info!("ASB_SYNCING_BITCOIN_WALLET");
        let start = util::get_sys_time_in_secs();
        wallet.sync().await?;
        let end = util::get_sys_time_in_secs();
        let duration = end - start;
        tracing::info!(%duration, "ASB_SYNCED_BITCOIN_WALLET");
        Ok(wallet)
    } else {
        wallet
    }
}

/// How long the ASB's Bitcoin wallet may take to connect to its Electrum server at start-up. Without a
/// limit, a server that never answered (two testnet onions on 2026-11-12) left the ASB waiting
/// forever with nothing in the log. A first onion connection can take a minute; this allows three.
const ELECTRUM_CONNECT_LIMIT: std::time::Duration = std::time::Duration::from_secs(180);

/// `connect`, or an error naming the Electrum server as the cause once `limit` has passed.
async fn within_electrum_connect_limit<T>(
    connect: impl std::future::Future<Output = Result<T>>,
    limit: std::time::Duration,
) -> Result<T> {
    match tokio::time::timeout(limit, connect).await {
        Ok(result) => result,
        Err(_) => bail!(
            "the Electrum server did not answer over Tor within {} s; pick another one in Settings",
            limit.as_secs()
        ),
    }
}

/// Waits between attempts at the ASB's Monero wallet sync at start-up (about 2.5 minutes in all).
/// The first sync of a new install goes through a Tor circuit to a remote node, and one slow or
/// dropped circuit used to end the ASB ("no connection to daemon", 2026-11-11) although the node was
/// fine a moment later. The GUI still restarts the whole ASB with backoff if every attempt fails.
fn monero_sync_retry_delays() -> Vec<std::time::Duration> {
    [10, 20, 40, 60].iter().map(|s| std::time::Duration::from_secs(*s)).collect()
}

/// Runs `attempt` until it succeeds, waiting `delays[i]` after failure `i`; returns the last error
/// once every delay is used up (so `delays.len() + 1` attempts in all).
async fn retry_monero_sync<F, Fut, T>(mut attempt: F, delays: &[std::time::Duration]) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut failures = 0;
    loop {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(error) if failures < delays.len() => {
                tracing::warn!(
                    attempt = failures + 1,
                    retry_in_secs = delays[failures].as_secs(),
                    error = %format!("{:#}", error),
                    "Monero wallet sync failed; retrying"
                );
                tokio::time::sleep(delays[failures]).await;
                failures += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn init_monero_wallet(
    config: &Config,
    env_config: swap::env::Config,
) -> Result<monero::Wallet> {
    let host = config
        .monero
        .wallet_rpc_url
        .host_str()
        .context("monero wallet RPC URL has no host")?;
    if host != "localhost" && host != "127.0.0.1" && host != "[::1]" && host != "::1" {
        bail!("monero wallet RPC must be a local endpoint; refusing to connect directly");
    }
    let wallet = monero::Wallet::open_or_create(
        config.monero.wallet_rpc_url.clone(),
        DEFAULT_WALLET_NAME.to_string(),
        env_config,
    )
    .await;
    if wallet.is_ok() {
        let wallet = wallet.unwrap();
        tracing::info!("ASB_INITIALIZED_MONERO_WALLET");
        tracing::info!("ASB_SYNCING_MONERO_WALLET");
        let start = util::get_sys_time_in_secs();
        let _ = retry_monero_sync(|| wallet.refresh(), &monero_sync_retry_delays()).await?;
        let _ = wallet.store().await; // save wallet upon sync
        let end = util::get_sys_time_in_secs();
        let duration = end - start;
        tracing::info!(%duration, "ASB_SYNCED_MONERO_WALLET");
        Ok(wallet)
    } else {
        wallet
    }
}

#[cfg(test)]
mod monero_sync_retry_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn a_sync_that_fails_twice_then_works_is_retried_to_success() {
        let calls = AtomicUsize::new(0);
        let result = retry_monero_sync(
            || async {
                if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                    bail!("no connection to daemon")
                }
                Ok(7)
            },
            &[Duration::ZERO; 4],
        )
        .await;
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_sync_that_keeps_failing_returns_the_error_after_every_retry() {
        let calls = AtomicUsize::new(0);
        let result: Result<()> = retry_monero_sync(
            || async {
                calls.fetch_add(1, Ordering::SeqCst);
                bail!("no connection to daemon")
            },
            &[Duration::ZERO; 4],
        )
        .await;
        assert!(format!("{:#}", result.unwrap_err()).contains("no connection to daemon"));
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn start_up_sync_is_retried_for_a_few_minutes_not_once() {
        let delays = monero_sync_retry_delays();
        assert!(delays.len() >= 3);
        let total: u64 = delays.iter().map(|d| d.as_secs()).sum();
        assert!((60..=300).contains(&total));
    }

    #[tokio::test(start_paused = true)]
    async fn an_electrum_server_that_never_answers_ends_with_a_clear_error() {
        let result: Result<()> =
            within_electrum_connect_limit(std::future::pending(), ELECTRUM_CONNECT_LIMIT).await;
        let message = format!("{:#}", result.unwrap_err());
        assert!(message.contains("Electrum server did not answer"));
        assert!(message.contains("180 s"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_connecting_electrum_server_is_not_cut_off() {
        let result = within_electrum_connect_limit(async { Ok(5) }, ELECTRUM_CONNECT_LIMIT).await;
        assert_eq!(result.unwrap(), 5);
    }

    #[test]
    fn the_connect_limit_allows_a_slow_first_onion_connection() {
        assert!(ELECTRUM_CONNECT_LIMIT >= Duration::from_secs(120));
    }
}
