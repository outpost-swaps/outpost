use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use libp2p::rendezvous;
use libp2p::swarm::SwarmEvent;
use libp2p::{identity, Multiaddr, Swarm};

/// Minimal standalone rendezvous server, namespace-agnostic (serves whatever
/// namespaces clients register under, e.g. mainnet and testnet at once).
/// Mirrors upstream's libp2p-rendezvous-node but built from the vendored
/// libp2p 0.54 stack we already ship.
#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = args
        .get(1)
        .map(|p| p.parse())
        .transpose()?
        .unwrap_or(8888);
    let secret_file = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "rendezvous-server-secret.key".to_string());

    let secret_bytes: [u8; 32] = match std::fs::read(&secret_file) {
        Ok(mut bytes) => {
            let secret = identity::ed25519::SecretKey::try_from_bytes(&mut bytes[0..32])?;
            secret.as_ref().try_into().expect("secret is 32 bytes")
        }
        Err(_) => {
            let keypair = identity::Keypair::generate_ed25519();
            let secret = identity::ed25519::SecretKey::from(
                keypair.try_into_ed25519().expect("generated keypair is ed25519"),
            );
            let secret_bytes: [u8; 32] = secret.as_ref().try_into().expect("secret is 32 bytes");
            std::fs::write(&secret_file, &secret_bytes)?;
            secret_bytes
        }
    };
    let identity = identity::Keypair::ed25519_from_bytes(secret_bytes)?;

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(identity)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default().nodelay(true),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )?
        .with_behaviour(|keypair| {
            rendezvous::server::Behaviour::new(rendezvous::server::Config::default())
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(600)))
        .build();

    let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{port}").parse()?;
    swarm.listen_on(listen_addr)?;
    let peer_id = *swarm.local_peer_id();
    tracing::info!(%peer_id, %port, "Rendezvous server listening (TCP). Wrap with an onion service for public use.");

    while let Some(event) = swarm.next().await {
        match event {
            SwarmEvent::Behaviour(rendezvous::server::Event::PeerRegistered {
                peer,
                registration,
            }) => {
                tracing::info!(
                    %peer,
                    namespace = %registration.namespace,
                    addresses = ?registration.record.addresses(),
                    "Peer registered"
                );
            }
            SwarmEvent::Behaviour(rendezvous::server::Event::RegistrationExpired(_)) => {}
            SwarmEvent::Behaviour(rendezvous::server::Event::DiscoverServed {
                enquirer, ..
            }) => {
                tracing::debug!(%enquirer, "Discovery served");
            }
            SwarmEvent::NewListenAddr { address, .. } => {
                tracing::info!(%address, "Listening");
            }
            _ => {}
        }
    }
    Ok(())
}
