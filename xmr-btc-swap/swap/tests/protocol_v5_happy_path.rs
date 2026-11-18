#![cfg(feature = "protocol-v5")]
pub mod harness;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use bitcoin::Amount;
use get_port::get_port;
use libp2p::core::Multiaddr;
use swap::asb::FixedRate as LegacyFixedRate;
use swap::env::GetConfig;
use swap::protocol_v5::alice::{self, AliceContext};
use swap::protocol_v5::core;
use swap::protocol_v5::db;
use swap::protocol_v5::event_loop::{
    BobEventLoop, MakerBehaviour, MakerEventLoop, MakerQuoteProvider, MakerSnapshot,
    MakerWalletSnapshotProvider,
};
use swap::protocol_v5::feed::FixedRate;
use swap::protocol_v5::machine::common::swap_fee_limits;
use swap::protocol_v5::p2p::protocols::quote::{BidQuote, RefundPolicyWire};
use swap::protocol_v5::p2p::protocols::swap_setup::alice::WalletSnapshot;
use swap::protocol_v5::p2p::protocols::swap_setup::bob::NewSwap;
use swap::protocol_v5::p2p::protocols::swap_setup::verification::LockInputVerifier;
use swap::protocol_v5::store::ProtocolV5Store;
use swap::seed::Seed;
use tempfile::tempdir;
use testcontainers::clients::Cli;
use tracing_subscriber::util::SubscriberInitExt;
use uuid::Uuid;

struct TestQuote(BidQuote);

#[async_trait::async_trait]
impl MakerQuoteProvider for TestQuote {
    async fn quote(&self) -> Result<BidQuote> {
        Ok(self.0.clone())
    }
}

struct TestSnapshot {
    bitcoin_wallet: Arc<swap::bitcoin::Wallet>,
    monero_wallet: Arc<swap::monero::Wallet>,
}

#[async_trait::async_trait]
impl MakerWalletSnapshotProvider for TestSnapshot {
    async fn snapshot(&self, btc_amount: core::bitcoin::Amount) -> Result<MakerSnapshot> {
        let balance = self.monero_wallet.get_balance().await?;
        let dojo_fee_rate = self.bitcoin_wallet.swap_fee_rate().await?;
        let fee_rate = ::bitcoin::FeeRate::from_sat_per_kwu(dojo_fee_rate.to_sat_per_kwu());
        let swap_fee_rate = core::bitcoin::SwapFeeRate::from_estimate(dojo_fee_rate)?;
        let fee_limits = swap_fee_limits(swap_fee_rate, btc_amount)?;
        let snapshot = WalletSnapshot::new(
            core::monero::Amount::from_pico(balance.unlocked_balance),
            self.bitcoin_wallet.new_address().await?,
            self.bitcoin_wallet.new_address().await?,
            fee_rate,
            swap_fee_rate,
            core::bitcoin::RedeemFeeMultiplier::try_from(250)?,
            fee_limits,
        );
        Ok(MakerSnapshot {
            snapshot,
            anti_spam_deposit: Amount::from_sat(0),
            hermes_enabled: false,
        })
    }

    async fn reserve_for_swap(
        &self,
        _swap_id: Uuid,
        _btc_amount: core::bitcoin::Amount,
        _xmr_amount: core::monero::Amount,
    ) -> Result<bool> {
        Ok(true)
    }

    async fn release_liquidity(&self, _swap_id: Uuid) {}
}

#[tokio::test]
async fn protocol_v5_happy_path_on_regtest() -> Result<()> {
    let cli = Cli::default();
    let _guard = tracing_subscriber::fmt()
        .with_env_filter(
            "warn,swap=debug,monero_harness=debug,monero_rpc=debug,testcontainers=info",
        )
        .with_test_writer()
        .set_default();

    let env_config = swap::env::Regtest::get_config();
    let (monero, containers) = harness::init_containers(&cli).await;
    monero.init_miner().await?;

    let electrs_rpc_port = containers
        .electrs
        .get_host_port(harness::electrs::RPC_PORT)
        .context("missing electrs port")?;
    let alice_seed = Seed::random()?;
    let bob_seed = Seed::random()?;

    let btc_amount = Amount::from_sat(1_000_000);
    let xmr_amount =
        swap::monero::Amount::from_monero(btc_amount.to_btc() / LegacyFixedRate::RATE)?;
    let alice_balances = harness::StartingBalances::new(Amount::ZERO, xmr_amount, Some(10));
    let bob_balances =
        harness::StartingBalances::new(btc_amount * 10, swap::monero::Amount::ZERO, None);

    let alice_wallet_dir = tempdir()?;
    let bob_wallet_dir = tempdir()?;
    let (alice_bitcoin, alice_monero) = harness::init_test_wallets(
        harness::MONERO_WALLET_NAME_ALICE,
        containers.bitcoind_url.clone(),
        &monero,
        alice_balances,
        alice_wallet_dir.path(),
        electrs_rpc_port,
        &alice_seed,
        env_config,
    )
    .await;
    let (bob_bitcoin, bob_monero) = harness::init_test_wallets(
        harness::MONERO_WALLET_NAME_BOB,
        containers.bitcoind_url.clone(),
        &monero,
        bob_balances,
        bob_wallet_dir.path(),
        electrs_rpc_port,
        &bob_seed,
        env_config,
    )
    .await;

    let dojo_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let dojo_addr = dojo_listener.local_addr()?;
    let dojo_requests = Arc::new(AtomicUsize::new(0));
    let dojo_server = {
        let dojo_requests = dojo_requests.clone();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = dojo_listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let _ = socket.peek(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf);
                let (status, body) = if head.contains("POST /auth/login") {
                    ("200 OK", "{\"authorizations\":{\"access_token\":\"test-token\"}}")
                } else if head.contains("GET /fees/estimator") {
                    dojo_requests.fetch_add(1, Ordering::SeqCst);
                    ("200 OK", "{\"0.1\":5,\"0.2\":5,\"0.5\":5,\"0.99\":5,\"0.999\":5}")
                } else if head.contains("GET /tx/") {
                    ("200 OK", "{\"confirmations\":0}")
                } else if head.contains("POST /pushtx/") {
                    // This mock does not relay to bitcoind. Fail the Dojo
                    // broadcast so the wallet uses its permitted Electrum
                    // broadcast fallback; production Dojo broadcast is covered
                    // by bitcoin-wallet unit tests.
                    ("503 Service Unavailable", "")
                } else {
                    panic!("unexpected Dojo request: {}", head.lines().next().unwrap_or(""));
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
        })
    };
    let dojo_url = url::Url::parse(&format!("http://{dojo_addr}"))?;
    alice_bitcoin
        .require_dojo_fee_estimator(dojo_url.clone(), "test-key".to_string(), 0)
        .await?;
    bob_bitcoin
        .require_dojo_fee_estimator(dojo_url, "test-key".to_string(), 0)
        .await?;

    monero.start_miner().await?;

    let protocol_config = swap::protocol_v5::protocol_config(env_config);
    let price = Amount::from_btc(0.01)?;
    let fee_limits = swap_fee_limits(core::bitcoin::SwapFeeRate(5_000), btc_amount)?;
    let quote = BidQuote {
        price,
        min_quantity: Amount::from_sat(1_000),
        max_quantity: Amount::ONE_BTC,
        refund_policy: RefundPolicyWire::FullRefund,
        reserve_proof: None,
    };

    let alice_identity = alice_seed.derive_libp2p_identity_asb();
    let alice_peer_id = alice_identity.public().to_peer_id();
    let maker_behaviour = MakerBehaviour::new(
        Amount::from_sat(1_000),
        Amount::ONE_BTC,
        protocol_config,
        FixedRate::default(),
        false,
        LockInputVerifier::spawn(Arc::new((*alice_bitcoin).clone())),
        alice_identity,
        Vec::new(),
        swap::protocol_v5::p2p::protocols::rendezvous::XmrBtcNamespace::Testnet,
        libp2p::connection_limits::ConnectionLimits::default(),
    );
    let mut maker_swarm = swap::protocol_v5::event_loop::new_maker_swarm(
        alice_seed.derive_libp2p_identity_asb(),
        get_port().ok_or_else(|| anyhow::anyhow!("no free port"))?,
        maker_behaviour,
    )
    .await?;
    let listen: Multiaddr = format!("/ip4/127.0.0.1/tcp/{}", get_port().ok_or_else(|| anyhow::anyhow!("no free port"))?).parse()?;
    maker_swarm.listen_on(listen.clone())?;

    let alice_dir = tempdir()?;
    let alice_db_path = alice_dir.path().join("alice.db");
    tokio::fs::File::create(&alice_db_path).await?;
    let alice_db = Arc::new(swap::database::SqliteDatabase::open(&alice_db_path).await?);
    let alice_store: Arc<dyn ProtocolV5Store> = alice_db.clone();

    let (maker_loop, maker_handle) = MakerEventLoop::new(
        maker_swarm,
        alice_store.clone(),
        Arc::new(TestQuote(quote.clone())),
        Arc::new(TestSnapshot {
            bitcoin_wallet: alice_bitcoin.clone(),
            monero_wallet: alice_monero.clone(),
        }),
    );
    let maker_task = tokio::spawn(maker_loop.run());

    let daemon = Arc::new(
        swap::monero::ng::Daemon::connect(
            &format!("http://127.0.0.1:{}", monero.monerod().rpc_port()),
            None,
            None,
        )
        .await?,
    );
    let alice_context = AliceContext {
        store: alice_store.clone(),
        bitcoin_wallet: alice_bitcoin.clone(),
        monero_wallet: alice_monero.clone(),
        monero_daemon: Some(daemon),
        env_config: protocol_config,
        monero_network: env_config.monero_network,
        liquidity: None,
    };
    let alice_task = tokio::spawn(alice::run_maker(maker_handle, alice_context));

    let mut bob_swarm = swap::protocol_v5::event_loop::new_bob_swarm(
        bob_seed.derive_libp2p_identity(),
        get_port().ok_or_else(|| anyhow::anyhow!("no free port"))?,
        protocol_config,
        bob_bitcoin.protocol_wallet(),
    )
    .await?;
    bob_swarm.add_peer_address(alice_peer_id, listen.clone());

    let bob_dir = tempdir()?;
    let bob_db_path = bob_dir.path().join("bob.db");
    tokio::fs::File::create(&bob_db_path).await?;
    let bob_db = Arc::new(swap::database::SqliteDatabase::open(&bob_db_path).await?);
    let bob_store: Arc<dyn ProtocolV5Store> = bob_db.clone();
    let (bob_loop, mut bob_handle) = BobEventLoop::new(bob_swarm, alice_peer_id, bob_store.clone(), listen.clone());
    let bob_task = tokio::spawn(bob_loop.run());

    let received_quote = bob_handle.request_quote().await?;
    assert_eq!(received_quote.price, quote.price);
    assert!(
        dojo_requests.load(Ordering::SeqCst) > 0,
        "the maker snapshot must fetch its advertised fee from Dojo"
    );

    let swap_id = Uuid::new_v4();
    let state2 = bob_handle
        .start_swap(NewSwap {
            swap_id,
            btc: btc_amount,
            tx_lock_fee: Amount::from_sat(1_000),
            fee_limits,
            bitcoin_refund_address: bob_bitcoin.new_address().await?,
            quoted_price: received_quote.price,
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

    let accepted_lock = Mutex::new(None);
    let confirmations = Mutex::new(Vec::new());
    let state4 = swap::protocol_v5::bob::run_until_signature_sent(
        &*bob_store,
        &mut bob_handle,
        swap_id,
        state3,
        |state4| *accepted_lock.lock().unwrap() = Some(state4.lock_transfer_proof().tx_hash().0),
        |txid: &str, count| confirmations.lock().unwrap().push((txid.to_owned(), count)),
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
    let lock_txid = state4.lock_transfer_proof().tx_hash().0;
    assert_eq!(*accepted_lock.lock().unwrap(), Some(lock_txid.clone()));
    let confirmations = confirmations.lock().unwrap();
    assert!(confirmations.iter().all(|(txid, _)| txid == &lock_txid));
    assert!(
        confirmations.last().is_some_and(|(_, count)| *count >= env_config.monero_finality_confirmations),
        "the successful finality watch must forward the actual final observation"
    );
    drop(confirmations);

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
                let redeem_phases = Mutex::new(Vec::new());
                swap::protocol_v5::bob::redeem_xmr_with_wallet(
                    &*bob_store,
                    swap_id,
                    &state5,
                    &*bob_monero,
                    bob_monero.get_main_address(),
                    None,
                    || redeem_phases.lock().unwrap().push("sync"),
                    || redeem_phases.lock().unwrap().push("sweep"),
                )
                .await?;
                assert_eq!(*redeem_phases.lock().unwrap(), vec!["sync", "sweep"]);
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
    assert!(matches!(bob_state, Some(db::bob::Bob::Done(_))));
    let alice_state = alice_store.load_alice(swap_id).await?;
    assert!(matches!(alice_state, Some(db::alice::Alice::Done(_))));

    maker_task.abort();
    alice_task.abort();
    bob_task.abort();
    dojo_server.abort();
    Ok(())
}
