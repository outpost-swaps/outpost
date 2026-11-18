use crate::asb::event_loop::LatestRate;
use crate::env;
use crate::network::quote::BidQuote;
use crate::network::rendezvous::{self, XmrBtcNamespace};
use crate::network::swap_setup::alice;
use crate::network::swap_setup::alice::WalletSnapshot;
use crate::network::transport::authenticate_and_multiplex;
use crate::network::tor_transport::TorDialOnlyTransport;
use crate::network::{encrypted_signature, quote, transfer_proof};
use crate::protocol::alice::State3;
use anyhow::{anyhow, Error, Result};
use libp2p::core::muxing::StreamMuxerBox;
use libp2p::core::transport::{Boxed, OptionalTransport};
use libp2p::request_response::ResponseChannel;
use libp2p::swarm::NetworkBehaviour;
use libp2p::websocket::WsConfig;
use libp2p::{connection_limits, dns, tcp};
use libp2p::{identify, identity, ping, Multiaddr, PeerId, Transport};
use std::time::Duration;
use uuid::Uuid;

pub mod transport {
    use super::*;

    /// Creates the libp2p transport for the ASB.
    pub fn new(
        identity: &identity::Keypair,
        maybe_tor_socks5_port: Option<u16>,
    ) -> Result<Boxed<(PeerId, StreamMuxerBox)>> {
        let tor_transport = || match maybe_tor_socks5_port {
            Some(port) => OptionalTransport::some(TorDialOnlyTransport::new(port)),
            None => OptionalTransport::none(),
        };

        let tcp = tcp::tokio::Transport::new(tcp::Config::new().nodelay(true));
        let tcp_with_dns = dns::tokio::Transport::system(tcp)?;

        let ws_inner_tcp = tcp::tokio::Transport::new(tcp::Config::new().nodelay(true));
        let ws_inner_tcp_dns = dns::tokio::Transport::system(ws_inner_tcp)?;

        // Match the CLI transport: WsConfig consumes ws/wss addresses before
        // the raw TCP/Tor transports see them, and its inner transport must
        // include Tor so /wss rendezvous points work through SOCKS.
        let websocket_with_dns = WsConfig::new(tor_transport().or_transport(ws_inner_tcp_dns));

        let transport = websocket_with_dns
            .or_transport(tor_transport())
            .or_transport(tcp_with_dns)
            .boxed();

        authenticate_and_multiplex(transport, identity)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use libp2p::core::transport::DialOpts;
        use libp2p::core::Transport;

        fn make_transport(maybe_tor_socks5_port: Option<u16>) -> Boxed<(PeerId, StreamMuxerBox)> {
            let identity = identity::Keypair::generate_ed25519();
            new(&identity, maybe_tor_socks5_port).unwrap()
        }

        fn dial_opts() -> DialOpts {
            DialOpts {
                role: libp2p::core::Endpoint::Dialer,
                port_use: libp2p::core::transport::PortUse::default(),
            }
        }

        #[tokio::test]
        async fn websocket_multiaddr_is_dialable_without_tor() {
            let ws_addr: Multiaddr = "/ip4/127.0.0.1/tcp/1/ws".parse().unwrap();
            assert!(make_transport(None).dial(ws_addr, dial_opts()).is_ok());
        }

        #[tokio::test]
        async fn wss_multiaddr_is_dialable_with_tor() {
            let wss_addr: Multiaddr = "/dns4/example.com/tcp/443/wss".parse().unwrap();
            assert!(
                make_transport(Some(9150)).dial(wss_addr, dial_opts()).is_ok(),
                "the Tor-backed websocket transport must claim wss addresses"
            );
        }
    }
}

pub mod behaviour {
    use libp2p::request_response::{
        InboundFailure, InboundRequestId, OutboundFailure, OutboundRequestId,
    };
    use libp2p::swarm::behaviour::toggle::Toggle;

    use super::*;

    #[allow(clippy::large_enum_variant)]
    #[derive(Debug)]
    pub enum OutEvent {
        SwapSetupInitiated {
            send_wallet_snapshot: bmrng::RequestReceiver<bitcoin::Amount, WalletSnapshot>,
        },
        SwapSetupCompleted {
            peer_id: PeerId,
            swap_id: Uuid,
            state3: State3,
            /// Key under which this swap's XMR is reserved in the shared
            /// [`crate::asb::liquidity::Reservations`]; the event loop re-keys
            /// it to `swap_id` (setup acceptance reserves before the buyer's
            /// swap id is known). `None` only if the setup that produced it
            /// somehow failed to reserve - a bug, logged where re-keyed.
            liquidity_key: Option<Uuid>,
        },
        SwapDeclined {
            peer: PeerId,
            error: alice::Error,
        },
        QuoteRequested {
            channel: ResponseChannel<BidQuote>,
            peer: PeerId,
        },
        TransferProofAcknowledged {
            peer: PeerId,
            id: OutboundRequestId,
        },
        EncryptedSignatureReceived {
            msg: encrypted_signature::Request,
            channel: ResponseChannel<()>,
            peer: PeerId,
        },
        Rendezvous(rendezvous::register::Event),
        OutboundRequestResponseFailure {
            peer: PeerId,
            error: OutboundFailure,
            request_id: OutboundRequestId,
            protocol: String,
        },
        InboundRequestResponseFailure {
            peer: PeerId,
            error: InboundFailure,
            request_id: InboundRequestId,
            protocol: String,
        },
        Failure {
            peer: PeerId,
            error: Error,
        },
        /// "Fallback" variant that allows the event mapping code to swallow
        /// certain events that we don't want the caller to deal with.
        Other,
    }

    impl OutEvent {
        pub fn unexpected_request(peer: PeerId) -> OutEvent {
            OutEvent::Failure {
                peer,
                error: anyhow!("Unexpected request received"),
            }
        }

        pub fn unexpected_response(peer: PeerId) -> OutEvent {
            OutEvent::Failure {
                peer,
                error: anyhow!("Unexpected response received"),
            }
        }
    }

    /// A `NetworkBehaviour` that represents an XMR/BTC swap node as Alice.
    ///
    /// Rendezvous registration uses the eigenwallet PR-#644 `register` behaviour
    /// (see notes/libp2p-upgrade-review.md §9) — addresses come from
    /// swarm-level `add_peer_address`, dispatch is gated on connection, failures
    /// surface as events with backoff retry.
    #[derive(NetworkBehaviour)]
    #[behaviour(out_event = "OutEvent", event_process = false)]
    #[allow(missing_debug_implementations)]
    pub struct Behaviour<LR>
    where
        LR: LatestRate + Send + 'static,
    {
        connection_limits: connection_limits::Behaviour,
        pub rendezvous: Toggle<rendezvous::register::Behaviour>,
        pub quote: quote::Behaviour,
        pub swap_setup: alice::Behaviour<LR>,
        pub transfer_proof: transfer_proof::Behaviour,
        pub encrypted_signature: encrypted_signature::Behaviour,
        pub identify: identify::Behaviour,

        /// Ping behaviour that ensures that the underlying network connection
        /// is still alive. If the ping fails a connection close event
        /// will be emitted that is picked up as swarm event.
        ping: ping::Behaviour,
    }

    impl<LR> Behaviour<LR>
    where
        LR: LatestRate + Send + 'static,
    {
        pub fn new(
            min_buy: bitcoin::Amount,
            max_buy: bitcoin::Amount,
            latest_rate: LR,
            resume_only: bool,
            env_config: env::Config,
            identify_params: (identity::Keypair, XmrBtcNamespace),
            rendezvous_nodes: Vec<PeerId>,
            connection_limits: connection_limits::ConnectionLimits,
            liquidity: std::sync::Arc<crate::asb::liquidity::Reservations>,
        ) -> Self {
            let (identity, namespace) = identify_params;
            let agent_version = format!("asb/{} ({})", env!("CARGO_PKG_VERSION"), namespace);
            let protocol_version = "/comit/xmr/btc/1.0.0".to_string();

            let identifyConfig = identify::Config::new(protocol_version, identity.public())
                .with_agent_version(agent_version);

            let pingConfig = ping::Config::new().with_timeout(Duration::from_secs(60));

            let behaviour = if rendezvous_nodes.is_empty() {
                None
            } else {
                Some(rendezvous::register::Behaviour::new(
                    identity,
                    rendezvous_nodes,
                    namespace.into(),
                ))
            };

            Self {
                connection_limits: connection_limits::Behaviour::new(connection_limits),
                rendezvous: Toggle::from(behaviour),
                quote: quote::asb(),
                swap_setup: alice::Behaviour::new(
                    min_buy,
                    max_buy,
                    env_config,
                    latest_rate,
                    resume_only,
                    liquidity,
                ),
                transfer_proof: transfer_proof::alice(),
                encrypted_signature: encrypted_signature::alice(),
                ping: ping::Behaviour::new(pingConfig),
                identify: identify::Behaviour::new(identifyConfig),
            }
        }
    }

    // connection_limits::Behaviour emits `Void` (it never produces events);
    // upstream handles this via `void::unreachable` (their swap-p2p out_event module).
    impl From<void::Void> for OutEvent {
        fn from(event: void::Void) -> Self {
            void::unreachable(event)
        }
    }

    impl From<ping::Event> for OutEvent {
        fn from(_: ping::Event) -> Self {
            OutEvent::Other
        }
    }

    impl From<identify::Event> for OutEvent {
        fn from(_: identify::Event) -> Self {
            OutEvent::Other
        }
    }

    impl From<rendezvous::register::Event> for OutEvent {
        fn from(event: rendezvous::register::Event) -> Self {
            OutEvent::Rendezvous(event)
        }
    }
}
