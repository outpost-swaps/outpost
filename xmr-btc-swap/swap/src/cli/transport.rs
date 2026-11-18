use crate::network::tor_transport::{LoopbackOnly, TorDialOnlyTransport};
use crate::network::transport::authenticate_and_multiplex;
use anyhow::Result;
use libp2p::core::muxing::StreamMuxerBox;
use libp2p::core::transport::{Boxed, OptionalTransport};
use libp2p::websocket::WsConfig;
use libp2p::{dns, identity, tcp, PeerId, Transport};

/// Creates the libp2p transport for the swap CLI.
///
/// The CLI's transport needs the following capabilities:
/// - Establish TCP connections
/// - Resolve DNS entries
/// - Dial onion-addresses through a running Tor daemon by connecting to the
///   socks5 port. If the port is not given, we will fall back to the regular
///   TCP transport.
/// - Dial websocket (wss) multiaddrs when Tor is not in use, mirroring the ASB
///   transport: several public rendezvous points are only reachable via
///   `wss/443` on clearnet.
/// The direct (non-Tor) arm of the transport chain.
///
/// Without Tor everything may be dialed directly. With Tor only loopback may
/// bypass it (a local ASB used for self-swap testing): every other address the
/// Tor arm does not recognise (`/dns6`, `/dnsaddr`, ...) is refused instead of
/// being resolved by the system resolver and dialed with the user's real IP.
fn direct_transport<T>(transport: T, tor_enabled: bool) -> Boxed<T::Output>
where
    T: Transport + Send + Unpin + 'static,
    T::Output: Send + 'static,
    T::Error: Send + Sync + 'static,
    T::Dial: Send + 'static,
    T::ListenerUpgrade: Send + 'static,
{
    if tor_enabled {
        LoopbackOnly(transport.boxed()).boxed()
    } else {
        transport.boxed()
    }
}

pub fn new(
    identity: &identity::Keypair,
    maybe_tor_socks5_port: Option<u16>,
) -> Result<Boxed<(PeerId, StreamMuxerBox)>> {
    let tor_transport = || match maybe_tor_socks5_port {
        Some(port) => OptionalTransport::some(TorDialOnlyTransport::new(port)),
        None => OptionalTransport::none(),
    };

    let tor_enabled = maybe_tor_socks5_port.is_some();

    let tcp = tcp::tokio::Transport::new(tcp::Config::new().nodelay(true));
    let tcp_with_dns = direct_transport(dns::tokio::Transport::system(tcp)?, tor_enabled);

    // WsConfig wraps a Tor-or-direct inner chain so that ws/wss dials get
    // TLS+websocket on top of Tor when Tor is configured (mirrors upstream
    // swap/src/cli/transport.rs).
    let ws_inner_tcp = tcp::tokio::Transport::new(tcp::Config::new().nodelay(true));
    let ws_inner_tcp_dns =
        direct_transport(dns::tokio::Transport::system(ws_inner_tcp)?, tor_enabled);
    let websocket_with_dns = WsConfig::new(tor_transport().or_transport(ws_inner_tcp_dns));

    // WsConfig only matches addresses ending in /ws or /wss, so it must come
    // first — otherwise the Tor or TCP arms eagerly claim the address (ignoring
    // the /ws suffix) and establish a raw connection without the WebSocket
    // handshake, which fails the upgrade against any real wss peer.
    let transport = websocket_with_dns
        .or_transport(tor_transport())
        .or_transport(tcp_with_dns)
        .boxed();

    authenticate_and_multiplex(transport, identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::core::Transport;
    use libp2p::core::transport::DialOpts;
    use libp2p::Multiaddr;

    fn make_transport(maybe_tor_socks5_port: Option<u16>) -> Boxed<(PeerId, StreamMuxerBox)> {
        let identity = identity::Keypair::generate_ed25519();
        new(&identity, maybe_tor_socks5_port).unwrap()
    }

    fn onion_addr() -> Multiaddr {
        // 56 valid base32 characters; connection is never actually attempted in
        // these tests, so the "node" does not need to exist.
        "/onion3/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:9841"
            .parse()
            .unwrap()
    }

    #[tokio::test]
    async fn websocket_multiaddr_is_dialable_without_tor() {
        let ws_addr: Multiaddr = "/ip4/127.0.0.1/tcp/1/ws".parse().unwrap();
        assert!(
            make_transport(None).dial(ws_addr, DialOpts { role: libp2p::core::Endpoint::Dialer, port_use: libp2p::core::transport::PortUse::default() }).is_ok(),
            "the plain-TCP transport chain must include websocket so wss rendezvous points are dialable without Tor"
        );
    }

    #[tokio::test]
    async fn plain_tcp_multiaddr_is_still_dialable_without_tor() {
        let tcp_addr: Multiaddr = "/ip4/127.0.0.1/tcp/1".parse().unwrap();
        assert!(make_transport(None).dial(tcp_addr, DialOpts { role: libp2p::core::Endpoint::Dialer, port_use: libp2p::core::transport::PortUse::default() }).is_ok());
    }

    #[tokio::test]
    async fn tor_transport_still_supports_localhost_fallback() {
        // The Tor arm rejects loopback multiaddrs ("certainly not reachable via
        // Tor"), and or_transport then falls back to the direct TCP transport —
        // intentional, so local services (monero-wallet-rpc) are dialed directly
        // instead of through Tor.
        let loopback_addr: Multiaddr = "/ip4/127.0.0.1/tcp/1".parse().unwrap();
        assert!(make_transport(Some(9150)).dial(loopback_addr, DialOpts { role: libp2p::core::Endpoint::Dialer, port_use: libp2p::core::transport::PortUse::default() }).is_ok());
    }

    fn dial_opts() -> DialOpts {
        DialOpts {
            role: libp2p::core::Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::default(),
        }
    }

    /// Whether the transport accepts `addr` for dialing at all (it never
    /// connects: the dial future is dropped without being polled).
    fn accepts(transport: &mut Boxed<(PeerId, StreamMuxerBox)>, addr: &str) -> bool {
        match transport.dial(addr.parse().unwrap(), dial_opts()) {
            Ok(_) => true,
            Err(libp2p::core::transport::TransportError::MultiaddrNotSupported(_)) => false,
            Err(other) => panic!("unexpected dial error for {addr}: {other:?}"),
        }
    }

    // Sellers register their own addresses at rendezvous points and the client
    // dials every one of them. The Tor arm only understands onion/ip4/ip6/dns/
    // dns4, and anything else used to fall through to a direct TCP + system DNS
    // dial, which reveals the user's real IP to the seller. With Tor configured
    // such addresses must be refused.
    #[tokio::test]
    async fn with_tor_a_dns6_address_is_refused_instead_of_dialed_directly() {
        let mut transport = make_transport(Some(9150));
        assert!(!accepts(&mut transport, "/dns6/attacker.example/tcp/9939"));
    }

    #[tokio::test]
    async fn with_tor_a_dnsaddr_address_is_refused_instead_of_resolved_and_dialed_directly() {
        let mut transport = make_transport(Some(9150));
        assert!(!accepts(&mut transport, "/dnsaddr/attacker.example"));
    }

    // The websocket wrapper dials its inner transport lazily (inside the dial
    // future), so `dial()` on the full chain cannot show that a `/wss` address
    // is refused. Its inner chain is Tor-or-`direct_transport`, so the property
    // is checked on `direct_transport` itself.
    #[tokio::test]
    async fn direct_transport_with_tor_only_dials_loopback() {
        let tcp = tcp::tokio::Transport::new(tcp::Config::new().nodelay(true));
        let mut direct = direct_transport(dns::tokio::Transport::system(tcp).unwrap(), true);
        let refused = |direct: &mut Boxed<_>, addr: &str| {
            matches!(
                direct.dial(addr.parse().unwrap(), dial_opts()),
                Err(libp2p::core::transport::TransportError::MultiaddrNotSupported(_))
            )
        };

        assert!(refused(&mut direct, "/dns6/attacker.example/tcp/443"));
        assert!(refused(&mut direct, "/dnsaddr/attacker.example"));
        assert!(refused(&mut direct, "/dns4/attacker.example/tcp/443"));
        assert!(refused(&mut direct, "/ip4/203.0.113.7/tcp/9939"));
        assert!(!refused(&mut direct, "/ip4/127.0.0.1/tcp/9939"));
        assert!(!refused(&mut direct, "/dns4/localhost/tcp/9939"));
    }

    #[tokio::test]
    async fn direct_transport_without_tor_dials_anything() {
        let tcp = tcp::tokio::Transport::new(tcp::Config::new().nodelay(true));
        let mut direct = direct_transport(dns::tokio::Transport::system(tcp).unwrap(), false);

        assert!(direct
            .dial("/dns6/example.org/tcp/443".parse().unwrap(), dial_opts())
            .is_ok());
    }

    #[tokio::test]
    async fn with_tor_public_addresses_are_still_accepted_through_tor() {
        let mut transport = make_transport(Some(9150));
        assert!(accepts(&mut transport, "/dns4/rendezvous.example/tcp/443"));
        assert!(accepts(&mut transport, "/dns/rendezvous.example/tcp/443"));
        assert!(accepts(&mut transport, "/ip4/203.0.113.7/tcp/9939"));
        assert!(accepts(&mut transport, "/dns4/rendezvous.example/tcp/443/wss"));
    }

    #[tokio::test]
    async fn with_tor_loopback_websocket_is_still_dialed_directly_for_a_local_asb() {
        let mut transport = make_transport(Some(9150));
        assert!(accepts(&mut transport, "/ip4/127.0.0.1/tcp/9939"));
        assert!(accepts(&mut transport, "/ip4/127.0.0.1/tcp/9939/ws"));
    }

    #[tokio::test]
    async fn without_tor_a_dns6_address_is_still_dialable() {
        let mut transport = make_transport(None);
        assert!(accepts(&mut transport, "/dns6/example.org/tcp/9939"));
    }

    #[tokio::test]
    async fn onion_multiaddr_is_dialable_with_tor() {
        // The SOCKS proxy on port 9150 does not exist; that makes the dial future
        // fail when awaited, but dial itself must accept the onion multiaddr.
        // (Address taken from the app's configured rendezvous list; never dialed.
        // Note: the onion3 multiaddr protocol takes the raw 56-char base32 host.)
        assert!(make_transport(Some(9150))
            .dial(
                "/onion3/yn5wziq6coomwvk2regzvwmjc2hvbw3z2w35bdfersr3kdqat6jwh6ad:8888"
                    .parse()
                    .unwrap(),
                DialOpts { role: libp2p::core::Endpoint::Dialer, port_use: libp2p::core::transport::PortUse::default() },
            )
            .is_ok());
    }
}
