#![cfg(feature = "protocol-v5")]

mod harness;

use std::path::Path;
use std::process::Stdio;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bitcoin::Amount;
use get_port::get_port;
use monero_rpc::monerod::MonerodRpc as _;
use monero_rpc::wallet::MoneroWalletRpc as _;
use rust_decimal::Decimal;
use swap::asb::config::{
    Bitcoin, Config, Data, Maker, Monero, Network as AsbNetwork, TorConf,
};
use swap::database::SqliteDatabase;
use swap::env::GetConfig;
use swap::protocol_v5::event_loop::{new_bob_swarm, BobEventLoop};
use swap::protocol_v5::machine::common::swap_fee_limits;
use swap::protocol_v5::p2p::protocols::swap_setup::bob::NewSwap;
use swap::protocol_v5::store::ProtocolV5Store;
use swap::seed::Seed;
use tempfile::tempdir;
use testcontainers::clients::Cli;
use tokio::io::AsyncWriteExt;
use url::Url;
use uuid::Uuid;

/// Process-boundary gate: the real `asb` binary must start against the
/// regtest containers with the regtest environment, direct local wallets,
/// and a real (mock) Dojo URL. This catches CLI/build/startup/DB failures
/// that the in-process harness cannot.
#[tokio::test(flavor = "multi_thread")]
async fn asb_binary_process_happy_path() -> Result<()> {
    let cli = Cli::default();
    let (monero, containers) = harness::init_containers(&cli).await;

    let alice_wallet = monero
        .wallet("alice")
        .context("missing Alice Monero wallet")?;
    let wallet_rpc_port = alice_wallet.rpc_port();
    monero.init_miner().await?;
    alice_wallet
        .client()
        .create_wallet("asb-wallet".to_owned(), "English".to_owned())
        .await
        .context("failed to create the ASB wallet")?;
    let asb_address = alice_wallet.address().await?.address;
    let miner = monero.wallet("miner").context("missing miner wallet")?;
    miner
        .transfer(&asb_address, 1_000_000_000_000)
        .await
        .context("failed to fund the ASB wallet")?;
    let miner_address = miner.address().await?.address;
    monero
        .monerod()
        .client()
        .generateblocks(10, miner_address)
        .await
        .context("failed to mine the ASB funding")?;
    alice_wallet.refresh().await?;
    alice_wallet
        .client()
        .close_wallet()
        .await
        .context("failed to close the ASB wallet before startup")?;

    let electrs_port = containers
        .electrs
        .get_host_port(harness::electrs::RPC_PORT)
        .context("missing electrs port")?;
    let monerod_port = monero.monerod().rpc_port();
    let listen_port = get_port().context("no free ASB listen port")?;

    let dir = tempdir()?;
    let config = Config {
        data: Data {
            dir: dir.path().to_path_buf(),
        },
        network: AsbNetwork {
            listen: vec![format!("/ip4/127.0.0.1/tcp/{listen_port}").parse()?],
            rendezvous_point: Vec::new(),
            external_addresses: Vec::new(),
        },
        bitcoin: Bitcoin {
            electrum_rpc_url: Url::parse(&format!("tcp://@localhost:{electrs_port}"))?,
            electrum_rpc_urls: vec![],
            target_block: 2,
            finality_confirmations: None,
            network: bitcoin::Network::Regtest,
        },
        monero: Monero {
            wallet_rpc_url: Url::parse(&format!(
                "http://127.0.0.1:{wallet_rpc_port}/json_rpc"
            ))?,
            daemon_url: Some(Url::parse(&format!("http://127.0.0.1:{monerod_port}"))?),
            finality_confirmations: None,
            network: monero::Network::Mainnet,
        },
        tor: TorConf {
            control_port: 0,
            socks5_port: 0,
        },
        maker: Maker {
            min_buy_btc: Amount::from_sat(1_000),
            max_buy_btc: Amount::from_sat(100_000),
            ask_spread: Decimal::ZERO,
            price_ticker_ws_url: Url::parse("ws://127.0.0.1:1")?,
            external_bitcoin_redeem_address: None,
            manual_price: Some(Decimal::from_str("0.01")?),
        },
    };
    let config_path = dir.path().join("asb.toml");
    std::fs::write(&config_path, toml::to_string(&config)?)?;

    let dojo_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let dojo_addr = dojo_listener.local_addr()?;
    let dojo_server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = dojo_listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = socket.peek(&mut buf).await.unwrap();
            let head = String::from_utf8_lossy(&buf);
            let (status, body) = if head.contains("POST /auth/login") {
                ("200 OK", "{\"authorizations\":{\"access_token\":\"test-token\"}}")
            } else if head.contains("GET /fees/estimator") {
                ("200 OK", "{\"0.1\":5,\"0.2\":5,\"0.5\":5,\"0.99\":5,\"0.999\":5}")
            } else if head.contains("GET /tx/") {
                ("200 OK", "{\"confirmations\":0}")
            } else if head.contains("POST /pushtx/") {
                // The mock does not relay to bitcoind. Fail so the wallet uses
                // its permitted Electrum broadcast fallback.
                ("503 Service Unavailable", "")
            } else {
                ("200 OK", "{}")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes())
                .await
                .unwrap();
        }
    });

    let dojo_url = Url::parse(&format!("http://{dojo_addr}"))?;
    let seed = Seed::random()?;
    let log_path = dir.path().join("asb-process.log");
    let mut child = spawn_asb(&config_path, &log_path, &seed, &dojo_url).await?;
    wait_asb_initialized(&mut child, &log_path).await?;

    let log = std::fs::read_to_string(&log_path)?;
    assert!(
        log.contains("ASB_INITIALIZED"),
        "ASB process log is missing ASB_INITIALIZED:\n{log}"
    );
    assert!(
        !log.contains("ASB_ERROR_INITIALIZING_XMR_WALLET"),
        "ASB failed to initialize the Monero wallet:\n{log}"
    );
    assert!(
        !log.contains("ASB_ERROR_INITIALIZING_BTC_WALLET"),
        "ASB failed to initialize the Bitcoin wallet:\n{log}"
    );

    // Crash/restart gate: the same seed, config, wallet, and SQLite state
    // must come back up cleanly before Bob starts the swap.
    let _ = child.kill().await;
    let _ = child.wait().await;
    let mut child = spawn_asb(&config_path, &log_path, &seed, &dojo_url).await?;
    wait_asb_initialized(&mut child, &log_path).await?;
    let restarted_log = std::fs::read_to_string(&log_path)?;
    assert!(
        restarted_log.contains("ASB_INITIALIZED"),
        "ASB did not initialize after restart:\n{restarted_log}"
    );
    assert!(
        !restarted_log.contains("ASB_ERROR_INITIALIZING_XMR_WALLET")
            && !restarted_log.contains("ASB_ERROR_INITIALIZING_BTC_WALLET"),
        "ASB failed wallet initialization after restart:\n{restarted_log}"
    );

    let env_config = swap::env::Regtest::get_config();
    let protocol_config = swap::protocol_v5::protocol_config(env_config);
    let bob_seed = Seed::random()?;
    let bob_balances = harness::StartingBalances::new(
        Amount::from_sat(1_000_000),
        swap::monero::Amount::ZERO,
        None,
    );
    let bob_dir = tempdir()?;
    let (bob_bitcoin, bob_monero) = harness::init_test_wallets(
        harness::MONERO_WALLET_NAME_BOB,
        containers.bitcoind_url.clone(),
        &monero,
        bob_balances,
        bob_dir.path(),
        electrs_port,
        &bob_seed,
        env_config,
    )
    .await;
    bob_bitcoin
        .require_dojo_fee_estimator(dojo_url.clone(), "test-key".to_owned(), 0)
        .await?;
    monero.start_miner().await?;

    let alice_peer_id = seed.derive_libp2p_identity_asb().public().to_peer_id();
    let listen_addr: libp2p::core::Multiaddr =
        format!("/ip4/127.0.0.1/tcp/{listen_port}/p2p/{alice_peer_id}").parse()?;
    let mut bob_swarm = new_bob_swarm(
        bob_seed.derive_libp2p_identity(),
        get_port().context("no free Bob swarm port")?,
        protocol_config,
        bob_bitcoin.protocol_wallet(),
    )
    .await?;
    bob_swarm.add_peer_address(alice_peer_id, listen_addr.clone());

    let bob_db_path = bob_dir.path().join("bob.db");
    std::fs::File::create(&bob_db_path)?;
    let bob_db = Arc::new(SqliteDatabase::open(&bob_db_path).await?);
    let bob_store: Arc<dyn ProtocolV5Store> = bob_db;
    let (bob_loop, mut bob_handle) = BobEventLoop::new(bob_swarm, alice_peer_id, bob_store.clone(), listen_addr.clone());
    let bob_task = tokio::spawn(bob_loop.run());

    let _quote = bob_handle.request_quote().await?;
    let btc_amount = Amount::from_sat(50_000);
    let fee_limits = swap_fee_limits(
        swap::protocol_v5::core::bitcoin::SwapFeeRate(5_000),
        btc_amount,
    )?;
    let swap_id = Uuid::new_v4();
    let state2 = bob_handle
        .start_swap(NewSwap {
            swap_id,
            btc: btc_amount,
            tx_lock_fee: Amount::from_sat(1_000),
            fee_limits,
            bitcoin_refund_address: bob_bitcoin.new_address().await?,
            // the test maker quotes manual_price 0.01 BTC per XMR
            quoted_price: Amount::from_sat(1_000_000),
        })
        .await?;
    bob_handle.commit().await?;

    let restore_height = swap::protocol_v5::core::monero::BlockHeight {
        height: bob_monero.block_height().await?.height as u64,
    };
    let (state3, _) = swap::protocol_v5::bob::prepare_lock(
        &*bob_store,
        swap_id,
        state2,
        restore_height,
        &**bob_bitcoin,
    )
    .await?;
    swap::protocol_v5::bob::publish_lock(&*bob_store, swap_id, &*bob_bitcoin).await?;
    let state4 = swap::protocol_v5::bob::run_until_signature_sent(
        &*bob_store,
        &mut bob_handle,
        swap_id,
        state3,
        |_state4| {},
        |_, _| {},
        || {},
        || {},
        &**bob_bitcoin,
        env_config.bitcoin_finality_confirmations,
        restore_height,
        &bob_monero,
        None,
        env_config.monero_network,
        env_config.monero_finality_confirmations,
    )
    .await?
    .expect("expected a transfer proof/state4 before redeem");

    let mut redeemed = false;
    for _ in 0..180 {
        match swap::protocol_v5::bob::observe_btc_outcome(
            &*bob_store,
            swap_id,
            &state4,
            &**bob_bitcoin,
        )
        .await?
        {
            swap::protocol_v5::bob::BobOutcome::XmrRedeemed(state5) => {
                swap::protocol_v5::bob::redeem_xmr_with_wallet(
                    &*bob_store,
                    swap_id,
                    &state5,
                    &bob_monero,
                    bob_monero.get_main_address(),
                    None,
                    || {},
                    || {},
                )
                .await?;
                redeemed = true;
                break;
            }
            swap::protocol_v5::bob::BobOutcome::Pending => {
                tokio::time::sleep(Duration::from_secs(1)).await
            }
            swap::protocol_v5::bob::BobOutcome::Cancelled(_) => {
                anyhow::bail!("Alice cancelled instead of redeeming")
            }
        }
    }
    assert!(redeemed, "Bob never observed Alice's redeem");
    let bob_state = bob_store.load_bob(swap_id).await?;
    assert!(matches!(
        bob_state,
        Some(swap::protocol_v5::db::bob::Bob::Done(_))
    ));
    let asb_deadline = Instant::now() + Duration::from_secs(30);
    let asb_log = loop {
        let log = std::fs::read_to_string(&log_path)?;
        if log.contains("ASB_SWAP_COMPLETE") {
            break log;
        }
        if Instant::now() >= asb_deadline {
            anyhow::bail!("the child ASB never completed the swap:\n{log}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert!(
        asb_log.contains("ASB_SWAP_COMPLETE"),
        "the child ASB never completed the swap:\n{asb_log}"
    );
    bob_task.abort();

    // Failure-path gate: reserve a second lock input, abort Bob before commit,
    // and require the ASB process to release the reservation.
    let asb_db_path = dir.path().join("sqlite");
    let mut bob_swarm = new_bob_swarm(
        bob_seed.derive_libp2p_identity(),
        get_port().context("no free Bob abort-swarm port")?,
        protocol_config,
        bob_bitcoin.protocol_wallet(),
    )
    .await?;
    bob_swarm.add_peer_address(alice_peer_id, listen_addr.clone());
    let (bob_loop, mut bob_handle) = BobEventLoop::new(bob_swarm, alice_peer_id, bob_store.clone(), listen_addr.clone());
    let bob_task = tokio::spawn(bob_loop.run());
    let _quote = bob_handle.request_quote().await?;
    let abort_swap_id = Uuid::new_v4();
    bob_handle
        .start_swap(NewSwap {
            swap_id: abort_swap_id,
            btc: btc_amount,
            tx_lock_fee: Amount::from_sat(1_000),
            fee_limits,
            bitcoin_refund_address: bob_bitcoin.new_address().await?,
            // the test maker quotes manual_price 0.01 BTC per XMR
            quoted_price: Amount::from_sat(1_000_000),
        })
        .await?;
    let reserved = wait_for_lock_input_count(&asb_db_path, 1, Duration::from_secs(10)).await?;
    assert_eq!(reserved, 1, "the ASB should reserve the second lock input");

    bob_task.abort();
    let released = wait_for_lock_input_count(&asb_db_path, 0, Duration::from_secs(10)).await?;
    assert_eq!(
        released, 0,
        "aborting Bob before commit must release the ASB reservation"
    );

    // Refund gate: Bob locks, publishes cancel, and refunds through the v5
    // state machine. Alice must observe the cancellation/refund and must not
    // punish a Bob who refunded inside the window.
    let mut bob_swarm = new_bob_swarm(
        bob_seed.derive_libp2p_identity(),
        get_port().context("no free Bob refund-swarm port")?,
        protocol_config,
        bob_bitcoin.protocol_wallet(),
    )
    .await?;
    bob_swarm.add_peer_address(alice_peer_id, listen_addr.clone());
    let (bob_loop, mut bob_handle) = BobEventLoop::new(bob_swarm, alice_peer_id, bob_store.clone(), listen_addr.clone());
    let bob_task = tokio::spawn(bob_loop.run());
    let _quote = bob_handle.request_quote().await?;
    let refund_swap_id = Uuid::new_v4();
    let state2 = bob_handle
        .start_swap(NewSwap {
            swap_id: refund_swap_id,
            btc: btc_amount,
            tx_lock_fee: Amount::from_sat(1_000),
            fee_limits,
            bitcoin_refund_address: bob_bitcoin.new_address().await?,
            // the test maker quotes manual_price 0.01 BTC per XMR
            quoted_price: Amount::from_sat(1_000_000),
        })
        .await?;
    bob_handle.commit().await?;
    let restore_height = swap::protocol_v5::core::monero::BlockHeight {
        height: bob_monero.block_height().await?.height as u64,
    };
    let (state3, _) = swap::protocol_v5::bob::prepare_lock(
        &*bob_store,
        refund_swap_id,
        state2,
        restore_height,
        &**bob_bitcoin,
    )
    .await?;
    swap::protocol_v5::bob::publish_lock(&*bob_store, refund_swap_id, &*bob_bitcoin).await?;
    let xmr_lock_marker = format!("ASB_XMR_LOCK_PROOF swap_id={refund_swap_id}");
    // The ASB's mainnet XMR construction cooldown is five minutes. The
    // refund swap follows the happy-path swap, so its lock can wait out the
    // happy-path slot before the proof is emitted.
    wait_for_log_marker(&log_path, &xmr_lock_marker, Duration::from_secs(420)).await?;
    let cancel_deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let expired = state3.expired_timelock(&**bob_bitcoin).await?;
        if !matches!(
            expired,
            swap::protocol_v5::core::bitcoin::ExpiredTimelocks::None { .. }
        ) {
            break;
        }
        if Instant::now() >= cancel_deadline {
            anyhow::bail!("cancel timelock did not mature for the refund scenario");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let state6 = swap::protocol_v5::bob::cancel_lock(
        &*bob_store,
        refund_swap_id,
        state3,
        restore_height,
    )
    .await?;

    // Regression: prepared is not broadcast, and published is not confirmed.
    // Record the actual callback/progress order while mining the regtest chain
    // so the cancel and refund can confirm.
    let progress = Arc::new(Mutex::new(Vec::<String>::new()));
    let cancel_broadcasts = Arc::new(Mutex::new(Vec::new()));
    let refund_broadcasts = Arc::new(Mutex::new(Vec::new()));
    let run = swap::protocol_v5::bob::run_cancel_and_refund(
        &*bob_store,
        refund_swap_id,
        state6,
        &**bob_bitcoin,
        |message| progress.lock().unwrap().push(message.to_string()),
        |txid| cancel_broadcasts.lock().unwrap().push(txid),
        |txid| refund_broadcasts.lock().unwrap().push(txid),
    );
    tokio::pin!(run);
    let mut mine_interval = tokio::time::interval(Duration::from_secs(5));
    let deadline = Instant::now() + Duration::from_secs(240);
    let refund_result = loop {
        if Instant::now() >= deadline {
            anyhow::bail!("cancel/refund did not complete after the cancel and refund confirmed");
        }
        tokio::select! {
            result = &mut run => break result,
            _ = mine_interval.tick() => {
                harness::mine_bitcoin_blocks(containers.bitcoind_url.clone(), 1).await?;
            }
        }
    };
    let refund_txid = match refund_result? {
        swap::protocol_v5::bob::RefundOutcome::Refunded(txid) => txid,
        other => anyhow::bail!("expected a full refund, got {other:?}"),
    };

    assert_eq!(
        cancel_broadcasts.lock().unwrap().len(),
        1,
        "the cancel-broadcast callback must fire exactly once, after acceptance"
    );
    assert_eq!(
        *refund_broadcasts.lock().unwrap(),
        vec![refund_txid],
        "the refund-broadcast callback must fire exactly once with the confirmed txid"
    );
    let progress = progress.lock().unwrap();
    let cancel_at = progress
        .iter()
        .position(|message| message.contains("cancel transaction broadcast"))
        .expect("cancel broadcast progress missing");
    let refund_at = progress
        .iter()
        .position(|message| message.contains("refund transaction broadcast"))
        .expect("refund broadcast progress missing");
    assert!(
        cancel_at < refund_at,
        "UI progress claimed the refund before the cancel broadcast: {progress:?}"
    );
    assert!(
        !progress.iter().any(|message| message.contains("refunded")),
        "the swap must not be reported as refunded before confirmation: {progress:?}"
    );

    let refunded_state = bob_store.load_bob(refund_swap_id).await?;
    assert!(
        matches!(
            refunded_state,
            Some(swap::protocol_v5::db::bob::Bob::Done(
                swap::protocol_v5::db::bob::BobEndState::BtcRefunded(_)
            ))
        ),
        "Bob must persist a confirmed refund terminal state, got: {refunded_state:?}"
    );
    let refund_marker = format!("ASB_SWAP_COMPLETE swap_id={refund_swap_id}");
    wait_for_log_marker(&log_path, &refund_marker, Duration::from_secs(120)).await?;
    bob_task.abort();

    // Punish gate: Bob locks and then goes silent. Alice must not wait forever
    // for the transfer-proof ack/encrypted signature; after the regtest
    // cancel/punish timelocks she must publish cancel and then punish.
    let mut bob_swarm = new_bob_swarm(
        bob_seed.derive_libp2p_identity(),
        get_port().context("no free Bob punish-swarm port")?,
        protocol_config,
        bob_bitcoin.protocol_wallet(),
    )
    .await?;
    bob_swarm.add_peer_address(alice_peer_id, listen_addr.clone());
    let (bob_loop, mut bob_handle) = BobEventLoop::new(bob_swarm, alice_peer_id, bob_store.clone(), listen_addr.clone());
    let bob_task = tokio::spawn(bob_loop.run());
    let _quote = bob_handle.request_quote().await?;
    let punish_swap_id = Uuid::new_v4();
    let state2 = bob_handle
        .start_swap(NewSwap {
            swap_id: punish_swap_id,
            btc: btc_amount,
            tx_lock_fee: Amount::from_sat(1_000),
            fee_limits,
            bitcoin_refund_address: bob_bitcoin.new_address().await?,
            // the test maker quotes manual_price 0.01 BTC per XMR
            quoted_price: Amount::from_sat(1_000_000),
        })
        .await?;
    bob_handle.commit().await?;
    let restore_height = swap::protocol_v5::core::monero::BlockHeight {
        height: bob_monero.block_height().await?.height as u64,
    };
    let (state3, _) = swap::protocol_v5::bob::prepare_lock(
        &*bob_store,
        punish_swap_id,
        state2,
        restore_height,
        &**bob_bitcoin,
    )
    .await?;
    swap::protocol_v5::bob::publish_lock(&*bob_store, punish_swap_id, &*bob_bitcoin).await?;
    let _ = state3;
    // Deliberately do not run run_until_signature_sent: Bob is "dead" after
    // locking, so Alice must reach the cancel/punish timelock path.
    let asb_log = wait_for_log_marker(
        &log_path,
        "ASB_PUNISH_TX",
        Duration::from_secs(700),
    )
    .await?;
    assert!(
        asb_log.contains("ASB_CANCEL_TX"),
        "the child ASB punished without publishing cancel:\n{asb_log}"
    );
    bob_task.abort();

    let _ = child.kill().await;
    let _ = child.wait().await;
    dojo_server.abort();
    Ok(())
}

async fn spawn_asb(
    config_path: &Path,
    log_path: &Path,
    seed: &Seed,
    dojo_url: &Url,
) -> Result<tokio::process::Child> {
    let log = std::fs::File::create(log_path)?;
    let stdout = Stdio::from(log.try_clone()?);
    let stderr = Stdio::from(log);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_asb"))
        .arg("--regtest")
        .arg("--seed-stdin")
        .arg("--config")
        .arg(config_path)
        .arg("start")
        .env("OUTPOST_DOJO_URL", dojo_url.to_string())
        .env("OUTPOST_DOJO_API_KEY", "test-key")
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .context("failed to spawn the real ASB binary")?;

    {
        let stdin = child.stdin.as_mut().context("ASB stdin was not piped")?;
        stdin.write_all(seed.to_base64().as_bytes()).await?;
        stdin.write_all(b"\n").await?;
    }
    drop(child.stdin.take());

    Ok(child)
}

async fn wait_asb_initialized(
    child: &mut tokio::process::Child,
    log_path: &Path,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let log = std::fs::read_to_string(log_path).unwrap_or_default();
        if log.contains("ASB_INITIALIZED") {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("ASB exited before initialization ({status}):\n{log}");
        }
        if Instant::now() >= deadline {
            let _ = child.kill().await;
            anyhow::bail!("ASB did not reach ASB_INITIALIZED:\n{log}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn wait_for_lock_input_count(path: &Path, expected: i64, timeout: Duration) -> Result<i64> {
    let deadline = Instant::now() + timeout;
    loop {
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", path.display())).await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM protocol_v5_lock_inputs")
            .fetch_one(&pool)
            .await?;
        pool.close().await;
        if count == expected {
            return Ok(count);
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "expected {expected} lock-input reservations, found {count} after {timeout:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_log_marker(path: &Path, marker: &str, timeout: Duration) -> Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let log = std::fs::read_to_string(path).unwrap_or_default();
        if log.contains(marker) {
            return Ok(log);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("log never contained {marker}:\n{log}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
