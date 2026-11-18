use crate::asb::LatestRate;
use crate::libp2p_ext::MultiAddrExt;
use crate::network::rendezvous::XmrBtcNamespace;
use crate::seed::Seed;
use crate::{asb, bitcoin, cli, env, tor};
use anyhow::{bail, Result};
use libp2p::connection_limits::ConnectionLimits;
use libp2p::swarm::NetworkBehaviour;
use libp2p::SwarmBuilder;
use libp2p::{identity, Multiaddr, PeerId, Swarm};
use std::fmt::Debug;
use std::time::Duration;

#[allow(clippy::too_many_arguments)]
pub async fn asb<LR>(
    seed: &Seed,
    min_buy: bitcoin::Amount,
    max_buy: bitcoin::Amount,
    latest_rate: LR,
    resume_only: bool,
    env_config: env::Config,
    namespace: XmrBtcNamespace,
    rendezvous_addrs: &[Multiaddr],
    tor_socks5_port: u16,
    liquidity: std::sync::Arc<crate::asb::liquidity::Reservations>,
) -> Result<Swarm<asb::Behaviour<LR>>>
where
    LR: LatestRate + Send + 'static + Debug + Clone,
{
    if tor_socks5_port == 0 {
        bail!("Tor is required: refusing to start the ASB without a SOCKS5 proxy");
    }
    let maybe_tor_socks5_port = Some(tor_socks5_port);

    let identity = seed.derive_libp2p_identity_asb();

    let rendezvous_nodes: Vec<PeerId> = rendezvous_addrs
        .iter()
        .map(|addr| {
            addr.extract_peer_id()
                .expect("Rendezvous node address must contain peer ID")
        })
        .collect();

    // DoS hardening: limits on inbound connections (upstream master numbers,
    // hardcoded there as well) — see notes/libp2p-upgrade-review.md §11.
    let connection_limits = ConnectionLimits::default()
        // Limit peers stuck in the handshake phase
        .with_max_pending_incoming(Some(64 * 4))
        .with_max_established_incoming(Some(128 * 4))
        // A single peer only needs one connection; allow 4 for brief overlap during reconnects
        .with_max_established_per_peer(Some(4));

    let behaviour = asb::Behaviour::new(
        min_buy,
        max_buy,
        latest_rate,
        resume_only,
        env_config,
        (identity.clone(), namespace),
        rendezvous_nodes,
        connection_limits,
        liquidity,
    );

    let transport = asb::transport::new(&identity, maybe_tor_socks5_port)?;

    let mut swarm = SwarmBuilder::with_existing_identity(identity)
        .with_tokio()
        .with_other_transport(|_| transport)?
        .with_behaviour(|_| behaviour)?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::MAX))
        .build();

    for addr in rendezvous_addrs {
        let peer_id = addr
            .extract_peer_id()
            .expect("Rendezvous node address must contain peer ID");

        swarm.add_peer_address(peer_id, addr.clone());
    }

    Ok(swarm)
}

pub async fn asb_with_behaviour<T>(
    identity: identity::Keypair,
    behaviour: T,
    rendezvous_addrs: &[Multiaddr],
    tor_socks5_port: u16,
) -> Result<Swarm<T>>
where
    T: NetworkBehaviour,
{
    if tor_socks5_port == 0 {
        bail!("Tor is required: refusing to start the ASB without a SOCKS5 proxy");
    }

    let transport = asb::transport::new(&identity, Some(tor_socks5_port))?;
    let mut swarm = SwarmBuilder::with_existing_identity(identity)
        .with_tokio()
        .with_other_transport(|_| transport)?
        .with_behaviour(|_| behaviour)?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::MAX))
        .build();

    for addr in rendezvous_addrs {
        let peer_id = addr
            .extract_peer_id()
            .expect("Rendezvous node address must contain peer ID");
        swarm.add_peer_address(peer_id, addr.clone());
    }

    Ok(swarm)
}

pub async fn cli<T>(
    identity: identity::Keypair,
    tor_socks5_port: u16,
    behaviour: T,
) -> Result<Swarm<T>>
where
    T: NetworkBehaviour,
{
    if tor_socks5_port == 0 {
        bail!("Tor is required: refusing to start the swap CLI without a SOCKS5 proxy");
    }
    let maybe_tor_socks5_port = Some(tor_socks5_port);

    let transport = cli::transport::new(&identity, maybe_tor_socks5_port)?;

    let swarm = SwarmBuilder::with_existing_identity(identity)
        .with_tokio()
        .with_other_transport(|_| transport)?
        .with_behaviour(|_| behaviour)?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::MAX))
        .build();

    Ok(swarm)
}
