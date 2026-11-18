use std::sync::Arc;
use std::str::FromStr;
use std::sync::Mutex;

use anyhow::{Context, Result};
use futures::StreamExt;
use libp2p::Swarm;
use url::Url;

use crate::asb::config::Config;
use crate::asb::liquidity::Reservations;
use crate::asb::v5_quote::{V5QuoteProvider, V5RateSource, V5SnapshotProvider};
use crate::asb::RateSource;
use crate::database::StateCipher;
use crate::protocol_v5::p2p::protocols::rendezvous::XmrBtcNamespace;
use crate::protocol_v5::alice::{self, AliceContext, LiquidityRelease};
use crate::protocol_v5::db;
use crate::protocol_v5::event_loop::{MakerBehaviour, MakerEventLoop, ResumeAliceSwap};
use crate::protocol_v5::machine::alice::AliceState;
use crate::protocol_v5::p2p::protocols::swap_setup::verification::LockInputVerifier;
use crate::protocol_v5::store::ProtocolV5Store;
use crate::seed::Seed;
use crate::database::SqliteDatabase;

pub async fn run(
    config: Config,
    seed: &Seed,
    env_config: crate::env::Config,
    testnet: bool,
    resume_only: bool,
    bitcoin_wallet: Arc<crate::bitcoin::Wallet>,
    monero_wallet: Arc<crate::monero::Wallet>,
    latest_rate: RateSource,
    proxy_string: String,
) -> Result<()> {
    let data_dir = config.data.dir.clone();
    let database = Arc::new(
        SqliteDatabase::open_with_cipher(
            data_dir.join("sqlite"),
            Some(StateCipher::from_seed(seed)),
        )
        .await?,
    );

    let rate = Arc::new(Mutex::new(latest_rate));
    let liquidity = Arc::new(Reservations::new());

    let quote_provider = Arc::new(V5QuoteProvider::new(
        rate.clone(),
        monero_wallet.clone(),
        liquidity.clone(),
        config.maker.min_buy_btc,
        config.maker.max_buy_btc,
    ));
    let snapshot_provider = Arc::new(V5SnapshotProvider::new(
        bitcoin_wallet.clone(),
        monero_wallet.clone(),
        config.maker.external_bitcoin_redeem_address.clone(),
        liquidity.clone(),
    ));

    let daemon_url = match config.monero.daemon_url.clone() {
        Some(url) => url,
        None => {
            let port = if env_config.monero_network == crate::monero::Network::Mainnet {
                18081
            } else {
                38081
            };
            let url = Url::parse(&format!("http://127.0.0.1:{port}"))
                .expect("static Monero daemon URL");
            tracing::warn!(
                %url,
                "ASB_MONERO_DAEMON_DEFAULTED: set monero.daemon_url to your monerod endpoint"
            );
            url
        }
    };
    let direct_regtest = env_config.bitcoin_network == bitcoin::Network::Regtest;
    let monero_proxy = if direct_regtest {
        None
    } else {
        Some(proxy_string.as_str())
    };
    let monero_daemon = Arc::new(
        crate::monero::ng::Daemon::connect(daemon_url.as_str(), monero_proxy, None)
            .await
            .context("Failed to connect to the Monero daemon for protocol 5")?,
    );

    let identity = seed.derive_libp2p_identity_asb();
    let namespace = XmrBtcNamespace::from_is_testnet(testnet);
    let rendezvous_nodes = config
        .network
        .rendezvous_point
        .iter()
        .filter_map(|address| crate::libp2p_ext::MultiAddrExt::extract_peer_id(address))
        .collect::<Vec<_>>();
    let connection_limits = libp2p::connection_limits::ConnectionLimits::default()
        .with_max_pending_incoming(Some(64 * 4))
        .with_max_established_incoming(Some(128 * 4))
        .with_max_established_per_peer(Some(4));

    tracing::info!("ASB_SETTING_UP_LIBP2P_SWARM");
    let verifier = LockInputVerifier::spawn(Arc::new((*bitcoin_wallet).clone()));
    let behaviour = MakerBehaviour::new(
        config.maker.min_buy_btc,
        config.maker.max_buy_btc,
        crate::protocol_v5::protocol_config(env_config),
        V5RateSource::new(rate.clone()),
        resume_only,
        verifier,
        identity.clone(),
        rendezvous_nodes,
        namespace,
        connection_limits,
    );
    let mut swarm = if direct_regtest {
        crate::protocol_v5::event_loop::new_maker_swarm(identity, 1, behaviour).await?
    } else {
        crate::network::swarm::asb_with_behaviour(
            identity,
            behaviour,
            &config.network.rendezvous_point,
            config.tor.socks5_port,
        )
        .await?
    };

    for listen in config.network.listen.clone() {
        if let Err(error) = Swarm::listen_on(&mut swarm, listen.clone()) {
            tracing::warn!(%listen, %error, "Failed to listen on configured ASB address");
        }
    }
    for external_address in config.network.external_addresses.clone() {
        Swarm::add_external_address(&mut swarm, external_address.clone());
        tracing::info!(%external_address, "ASB_REGISTERING_ADDRESS_WITH_RENDEZVOUS");
    }

    let (event_loop, mut handle) = MakerEventLoop::new(
        swarm,
        database.clone(),
        quote_provider,
        snapshot_provider.clone(),
    );
    let peer_id = event_loop.peer_id();
    // Resume requests are answered by the event loop, so it must already be
    // running before the startup resume loop sends any. Awaiting `run()` at
    // the end of this function used to deadlock ASB startup whenever a swap
    // needed resuming.
    let event_loop_task = tokio::spawn(event_loop.run());

    let context = AliceContext {
        store: database.clone(),
        bitcoin_wallet: bitcoin_wallet.clone(),
        monero_wallet: monero_wallet.clone(),
        monero_daemon: Some(monero_daemon),
        env_config: crate::protocol_v5::protocol_config(env_config),
        monero_network: env_config.monero_network,
        liquidity: Some(snapshot_provider as Arc<dyn LiquidityRelease>),
    };

    let stored = database
        .all_protocol_v5_states("alice")
        .await?
        .into_iter()
        .map(|(swap_id, value, _)| (swap_id, value))
        .collect::<Vec<_>>();
    // One bad row (a corrupt peer id, a failed resume) used to abort the whole
    // startup with `?`, leaving every later swap unwatched (audit finding A-2).
    let mut resumer = StartupResumer {
        database: database.clone(),
        liquidity: liquidity.clone(),
        context: context.clone(),
        handle: &mut handle,
    };
    let failures = resume_all(&mut resumer, stored).await;
    if failures > 0 {
        tracing::warn!(
            failures,
            "ASB_V5_RESUME_INCOMPLETE: some stored swaps were not resumed and need manual attention"
        );
    }

    let monero_balance = monero_wallet.get_balance().await?;
    let monero_address = monero_wallet.get_main_address();
    let bitcoin_balance = bitcoin_wallet.balance().await?.to_sat();
    let multiaddr = config
        .network
        .external_addresses
        .first()
        .map(ToString::to_string)
        .unwrap_or_default();
    tracing::info!(
        asb_peer_id = %peer_id,
        %multiaddr,
        %monero_address,
        %monero_balance.balance,
        %monero_balance.unlocked_balance,
        %bitcoin_balance,
        "ASB_INITIALIZED"
    );

    let balance_wallet = monero_wallet.clone();
    tokio::spawn(async move {
        loop {
            match balance_wallet.get_balance().await {
                Ok(balance) => tracing::info!(
                    %balance.balance,
                    %balance.unlocked_balance,
                    "ASB_XMR_BALANCE_DATA"
                ),
                Err(error) => tracing::info!(
                    error = %error,
                    "ASB_XMR_BALANCE_DATA"
                ),
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });

    tokio::spawn(alice::run_maker(handle, context));
    event_loop_task
        .await
        .context("Protocol-5 maker event loop stopped unexpectedly")?;
    Ok(())
}

/// Resumes one stored swap at startup.
#[async_trait::async_trait]
pub(crate) trait SwapResumer {
    async fn resume(&mut self, swap_id: uuid::Uuid, value: serde_json::Value) -> Result<()>;
}

/// Resumes every stored swap. A failure for one swap is logged and does not stop
/// the others; returns the number of swaps that failed.
pub(crate) async fn resume_all<R: SwapResumer + Send>(
    resumer: &mut R,
    stored: Vec<(uuid::Uuid, serde_json::Value)>,
) -> usize {
    let mut failures = 0;
    for (swap_id, value) in stored {
        if let Err(error) = resumer.resume(swap_id, value).await {
            failures += 1;
            tracing::warn!(%swap_id, "ASB_V5_RESUME_FAILED: {error:#}");
        }
    }
    failures
}

struct StartupResumer<'a> {
    database: Arc<SqliteDatabase>,
    liquidity: Arc<Reservations>,
    context: AliceContext,
    handle: &'a mut crate::protocol_v5::event_loop::MakerEventLoopHandle,
}

#[async_trait::async_trait]
impl SwapResumer for StartupResumer<'_> {
    async fn resume(&mut self, swap_id: uuid::Uuid, value: serde_json::Value) -> Result<()> {
        let Ok(db_state) = serde_json::from_value::<db::alice::Alice>(value) else {
            tracing::warn!(%swap_id, "ASB_V5_RESUME_SKIP: unreadable Alice state");
            return Ok(());
        };
        let state: AliceState = db_state.into();
        if crate::protocol_v5::machine::alice::is_complete(&state) {
            self.database.release_lock_inputs(swap_id).await?;
            self.liquidity.release(swap_id);
            return Ok(());
        }
        let peer_id = match self.database.load_alice_peer_id(swap_id).await? {
            Some(peer_id) => libp2p::PeerId::from_str(&peer_id)
                .with_context(|| format!("Invalid stored peer id for {swap_id}"))?,
            None => {
                tracing::warn!(%swap_id, "ASB_V5_RESUME_SKIP: missing peer id");
                return Ok(());
            }
        };
        let Some(state3) = alice_state3(&state) else {
            tracing::warn!(%swap_id, "ASB_V5_RESUME_SKIP: state has no lock state");
            return Ok(());
        };
        let reserved = crate::monero::Amount::from_piconero(state3.xmr.as_pico());
        // Say it plainly: an unfinished swap found at startup is being picked up
        // again. Without this the log only showed the liquidity re-reservation.
        tracing::info!(%swap_id, state = %state, "ASB_SWAP_RESUMING");
        self.liquidity
            .force_reserve(swap_id, reserved, crate::monero::MONERO_FEE);
        tracing::info!(%swap_id, amount = %reserved, "ASB_LIQUIDITY_RERESERVED");
        let registered = self
            .handle
            .resume_swap(ResumeAliceSwap {
                swap_id,
                peer_id,
                state3,
            })
            .await?;
        let context = self.context.clone();
        let liquidity = context.liquidity.clone();
        tokio::spawn(async move {
            if let Err(error) = alice::run_from(context, registered, state).await {
                tracing::warn!(%swap_id, "ASB_V5_RESUME_FAILED: {error:#}");
                if let Some(liquidity) = liquidity {
                    liquidity.release(swap_id);
                }
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod resume_tests {
    use super::*;

    struct MockResumer {
        attempted: Vec<uuid::Uuid>,
        fail_on: uuid::Uuid,
    }

    #[async_trait::async_trait]
    impl SwapResumer for MockResumer {
        async fn resume(&mut self, swap_id: uuid::Uuid, _value: serde_json::Value) -> Result<()> {
            self.attempted.push(swap_id);
            if swap_id == self.fail_on {
                anyhow::bail!("Invalid stored peer id for {swap_id}");
            }
            Ok(())
        }
    }

    // Audit A-2: the first failing row used to abort the whole startup resume
    // with `?`, so every later swap was never watched.
    #[tokio::test]
    async fn a_failing_swap_does_not_stop_the_others_from_resuming() {
        let ids: Vec<uuid::Uuid> = (1..=4).map(uuid::Uuid::from_u128).collect();
        let mut resumer = MockResumer {
            attempted: Vec::new(),
            fail_on: ids[1],
        };
        let stored = ids
            .iter()
            .map(|id| (*id, serde_json::Value::Null))
            .collect::<Vec<_>>();

        let failures = resume_all(&mut resumer, stored).await;

        assert_eq!(failures, 1);
        assert_eq!(resumer.attempted, ids, "every stored swap must be attempted");
    }

    #[tokio::test]
    async fn resuming_with_no_failures_reports_zero() {
        let id = uuid::Uuid::from_u128(9);
        let mut resumer = MockResumer {
            attempted: Vec::new(),
            fail_on: uuid::Uuid::from_u128(0),
        };

        let failures = resume_all(&mut resumer, vec![(id, serde_json::Value::Null)]).await;

        assert_eq!(failures, 0);
        assert_eq!(resumer.attempted, vec![id]);
    }
}

fn alice_state3(state: &AliceState) -> Option<crate::protocol_v5::machine::alice::State3> {
    match state {
        AliceState::Started { state3 }
        | AliceState::BtcLockTransactionSeen { state3 }
        | AliceState::BtcLocked { state3 }
        | AliceState::XmrReadyToLock { state3, .. }
        | AliceState::BtcEarlyRefundable { state3 }
        | AliceState::XmrLockTransactionConstructed { state3, .. }
        | AliceState::XmrLockTransactionSent { state3, .. }
        | AliceState::XmrLocked { state3, .. }
        | AliceState::XmrLockTransferProofSent { state3, .. }
        | AliceState::EncSigLearned { state3, .. }
        | AliceState::BtcRedeemTransactionPublished { state3, .. }
        | AliceState::BtcCancelled { state3, .. }
        | AliceState::BtcEarlyRefunded(state3)
        | AliceState::BtcRefunded { state3, .. }
        | AliceState::BtcPartiallyRefunded { state3, .. }
        | AliceState::XmrRefundable { state3, .. }
        | AliceState::XmrRefundTxConstructed { state3, .. }
        | AliceState::XmrRefundTxPublished { state3, .. }
        | AliceState::BtcWithholdPublished { state3 }
        | AliceState::BtcWithholdConfirmed { state3 }
        | AliceState::BtcPunishable { state3, .. } => Some((**state3).clone()),
        _ => None,
    }
}
