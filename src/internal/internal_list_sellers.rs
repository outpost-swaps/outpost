use swap::libp2p_ext::MultiAddrExt;
use swap::network::quote::BidQuote;
use swap::network::rendezvous::XmrBtcNamespace;
use swap::network::{quote, swarm};
use anyhow::{Context, Result};
use futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::request_response;
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identity, ping, rendezvous, Multiaddr, PeerId, Swarm};
use serde::Serialize;
use serde_with::{serde_as, DisplayFromStr};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::time::Duration;

/// How long direct-maker quoting waits for dial-in-flight makers before
/// returning whatever quotes were received.
const DISCOVERY_DEADLINE: Duration = Duration::from_secs(90);

/// Seller addresses must carry the peer ID so the buyer's swap swarm can dial
/// them and the GUI can recognize the local ASB.
fn seller_multiaddr(peer_id: PeerId, address: &Multiaddr) -> Multiaddr {
    let suffix = Protocol::P2p(peer_id);
    if address.ends_with(&Multiaddr::empty().with(suffix.clone())) {
        address.clone()
    } else {
        address.clone().with(suffix)
    }
}


/// Returns sorted list of sellers, with [Online](Status::Online) listed first.
///
/// First uses the rendezvous node to discover peers in the given namespace,
/// then fetches a quote from each peer that was discovered. If fetching a quote
/// from a discovered peer fails the seller's status will be
/// [Unreachable](Status::Unreachable).
pub async fn internal_list_sellers(
    rendezvous_node_peer_id: PeerId,
    rendezvous_node_addr: Multiaddr,
    namespace: XmrBtcNamespace,
    tor_socks5_port: u16,
    identity: identity::Keypair,
) -> Result<Vec<Seller>> {
    let behaviour = Behaviour {
        rendezvous: rendezvous::client::Behaviour::new(identity.clone()),
        quote: quote::cli(),
        ping: ping::Behaviour::new(ping::Config::new().with_timeout(Duration::from_secs(60))),
    };
    let mut swarm = swarm::cli(identity, tor_socks5_port, behaviour).await?;

    swarm.add_peer_address(rendezvous_node_peer_id, rendezvous_node_addr.clone());

    swarm
        .dial(DialOpts::from(rendezvous_node_peer_id))
        .context("Failed to dial rendezvous node")?;

    let event_loop = EventLoop::new(
        swarm,
        rendezvous_node_peer_id,
        rendezvous_node_addr,
        namespace,
    );
    let sellers = event_loop.run(tor_socks5_port).await;

    Ok(sellers)
}

/// Quotes a list of known maker addresses directly, without rendezvous
/// discovery. Each address must end in a `/p2p/<peer-id>` suffix. Keeps maker
/// discovery functional while all rendezvous points are unreachable.
pub async fn internal_list_sellers_direct(
    maker_addresses: Vec<Multiaddr>,
    tor_socks5_port: u16,
    identity: identity::Keypair,
) -> Result<Vec<Seller>> {
    let behaviour = Behaviour {
        rendezvous: rendezvous::client::Behaviour::new(identity.clone()),
        quote: quote::cli(),
        ping: ping::Behaviour::new(ping::Config::new().with_timeout(Duration::from_secs(60))),
    };
    let mut swarm = swarm::cli(identity, tor_socks5_port, behaviour).await?;

    let mut event_loop = EventLoop::new_direct(swarm);
    event_loop.seed_makers(maker_addresses);
    let sellers = event_loop.run(tor_socks5_port).await;

    Ok(sellers)
}

#[serde_as]
#[derive(Debug, Serialize, PartialEq, Eq, Hash, Clone)]
pub struct Seller {
    pub status: Status,
    #[serde_as(as = "DisplayFromStr")]
    pub multiaddr: Multiaddr,
}

impl Ord for Seller {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.status
            .cmp(&other.status)
            .then_with(|| self.multiaddr.to_string().cmp(&other.multiaddr.to_string()))
    }
}

impl PartialOrd for Seller {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Serialize, PartialEq, Eq, Hash, Clone)]
pub enum Status {
    Online(BidQuote),
    Unreachable,
}

impl Ord for Status {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (self, other) {
            (Status::Online(a), Status::Online(b)) => a.price.cmp(&b.price),
            (Status::Online(_), Status::Unreachable) => Ordering::Less,
            (Status::Unreachable, Status::Online(_)) => Ordering::Greater,
            (Status::Unreachable, Status::Unreachable) => Ordering::Equal,
        }
    }
}

impl PartialOrd for Status {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug)]
enum OutEvent {
    Rendezvous(rendezvous::client::Event),
    Quote(quote::OutEvent),
    Ping(ping::Event),
}

impl From<rendezvous::client::Event> for OutEvent {
    fn from(event: rendezvous::client::Event) -> Self {
        OutEvent::Rendezvous(event)
    }
}

impl From<quote::OutEvent> for OutEvent {
    fn from(event: quote::OutEvent) -> Self {
        OutEvent::Quote(event)
    }
}

#[derive(NetworkBehaviour)]
#[behaviour(event_process = false)]
#[behaviour(out_event = "OutEvent")]
struct Behaviour {
    rendezvous: rendezvous::client::Behaviour,
    quote: quote::Behaviour,
    ping: ping::Behaviour,
}

#[derive(Debug)]
enum QuoteStatus {
    Pending,
    Received(Status),
}

#[derive(Debug)]
enum State {
    WaitForDiscovery,
    WaitForQuoteCompletion,
}

/// Quote-phase bookkeeping for discovered/dialed makers, kept separate from
/// the swarm so deadline behavior can be tested hermetically.
#[derive(Debug, Default)]
struct QuoteTracker {
    reachable_asb_address: HashMap<PeerId, Multiaddr>,
    unreachable_asb_address: HashMap<PeerId, Multiaddr>,
    asb_quote_status: HashMap<PeerId, QuoteStatus>,
}

impl QuoteTracker {
    /// Sellers whose quote status is settled (online or unreachable), with
    /// their best-known address. Pending entries are dropped — this is what
    /// the hard deadline reports, so unreachable makers can never wedge the
    /// result.
    fn settled_sellers(&self) -> Vec<Seller> {
        self.asb_quote_status
            .iter()
            .filter_map(|(peer_id, quote_status)| match quote_status {
                QuoteStatus::Pending => None,
                QuoteStatus::Received(Status::Online(quote)) => self
                    .reachable_asb_address
                    .get(peer_id)
                    .map(|address| Seller {
                        multiaddr: address.clone(),
                        status: Status::Online(quote.clone()),
                    }),
                QuoteStatus::Received(Status::Unreachable) => self
                    .unreachable_asb_address
                    .get(peer_id)
                    .map(|address| Seller {
                        multiaddr: address.clone(),
                        status: Status::Unreachable,
                    }),
            })
            .collect()
    }
}

struct EventLoop {
    swarm: Swarm<Behaviour>,
    rendezvous_peer_id: Option<PeerId>,
    rendezvous_addr: Option<Multiaddr>,
    namespace: Option<XmrBtcNamespace>,
    quote_tracker: QuoteTracker,
    state: State,
}

impl EventLoop {
    fn new(
        swarm: Swarm<Behaviour>,
        rendezvous_peer_id: PeerId,
        rendezvous_addr: Multiaddr,
        namespace: XmrBtcNamespace,
    ) -> Self {
        Self {
            swarm,
            rendezvous_peer_id: Some(rendezvous_peer_id),
            rendezvous_addr: Some(rendezvous_addr),
            namespace: Some(namespace),
            quote_tracker: Default::default(),
            state: State::WaitForDiscovery,
        }
    }

    fn new_direct(swarm: Swarm<Behaviour>) -> Self {
        Self {
            swarm,
            rendezvous_peer_id: None,
            rendezvous_addr: None,
            namespace: None,
            quote_tracker: Default::default(),
            state: State::WaitForQuoteCompletion,
        }
    }

    /// Registers known maker addresses for direct quoting: adds their address
    /// to the swarm and starts dialing. The quote request is sent once the
    /// connection is established (see the `ConnectionEstablished` arm).
    fn seed_makers(&mut self, maker_addresses: Vec<Multiaddr>) {
        for maker_addr in maker_addresses {
            let peer_id = match maker_addr.extract_peer_id() {
                Some(peer_id) => peer_id,
                None => {
                    tracing::warn!(%maker_addr, "Known maker address has no peer id, skipping");
                    continue;
                }
            };
            self.swarm.add_peer_address(peer_id, maker_addr.clone());
            self.quote_tracker.asb_quote_status
                .insert(peer_id, QuoteStatus::Pending);
            if let Err(error) = self.swarm.dial(DialOpts::from(peer_id)) {
                tracing::warn!(%peer_id, "Failed to dial known maker: {:#}", error);
            }
        }
    }

    async fn run(mut self, proxy_port: u16) -> Vec<Seller> {
        // Both modes need a cap: unreachable makers would otherwise keep
        // Pending entries alive forever. Direct mode has no discovery step to
        // wait on; rendezvous mode still spends most of its budget dialing and
        // quoting discovered makers, so its deadline must comfortably cover
        // discovery + quote round-trips (live probe: ~3s).
        let hard_deadline = Some(tokio::time::Instant::now() + DISCOVERY_DEADLINE);
        let deadline_fut = async {
            match hard_deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(deadline_fut);
        loop {
            tokio::select! {
                _ = &mut deadline_fut, if hard_deadline.is_some() => {
                    // Time's up: makers still Pending (dial in flight) are dropped
                    // silently; anything already Received is reported.
                    let mut sellers = self.quote_tracker.settled_sellers();
                    sellers.sort();
                    break sellers;
                }
                swarm_event = self.swarm.select_next_some() => {
                    match swarm_event {
                        SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                            if self.rendezvous_peer_id == Some(peer_id) {
                                tracing::info!(
                                    "Connected to rendezvous point, discovering nodes in '{}' namespace ...",
                                    self.namespace.clone().expect("rendezvous mode always has a namespace")
                                );

                                self.swarm.behaviour_mut().rendezvous.discover(
                                    Some(rendezvous::Namespace::new(self.namespace.clone().expect("rendezvous mode always has a namespace").to_string()).expect("our namespace to be a correct string")),
                                    None,
                                    None,
                                    self.rendezvous_peer_id.expect("rendezvous mode always has a rendezvous peer"),
                                );
                            } else {
                                let address = seller_multiaddr(peer_id, &endpoint.get_remote_address());
                                tracing::debug!(%peer_id, %address, "Connection established to peer");
                                self.quote_tracker.reachable_asb_address.insert(peer_id, address);
                                if self.rendezvous_peer_id.is_none() {
                                    // Direct mode: the connection is up, now fetch the quote.
                                    let _request_id = self.swarm.behaviour_mut().quote.send_request(&peer_id, ());
                                }
                            }
                        }
                        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                            if let Some(peer_id) = peer_id {
                                if self.rendezvous_peer_id == Some(peer_id) {
                                    // warn, not error: one unreachable rendezvous point over Tor is
                                    // routine; only all of them down empties the seller list
                                    tracing::warn!(
                                        %peer_id,
                                        rendezvous_addr = %self.rendezvous_addr.clone().unwrap_or_else(Multiaddr::empty),
                                        "Failed to connect to rendezvous point: {}",
                                        error
                                    );

                                    // if the rendezvous node is unreachable we just stop
                                    return Vec::new();
                                } else {
                                    // debug: an unreachable seller over Tor is routine on every refresh,
                                    // and the swap log keeps only atomicswap=info, so these stay out of
                                    // a finished swap's log file; the seller list marks it unreachable
                                    tracing::debug!(
                                        %peer_id,
                                        "Failed to connect to peer: {}",
                                        error
                                    );
                                    self.quote_tracker.unreachable_asb_address.insert(peer_id, Multiaddr::empty());

                                    match self.quote_tracker.asb_quote_status.entry(peer_id) {
                                        Entry::Occupied(mut entry) => {
                                            entry.insert(QuoteStatus::Received(Status::Unreachable));
                                        },
                                        _ => {
                                            tracing::debug!(%peer_id, %error, "Connection error with unexpected peer");
                                        }
                                    }
                                }
                            } else {
                                tracing::debug!("Failed to connect to peer with no peer id");
                            }
                        }
                        SwarmEvent::Behaviour(OutEvent::Rendezvous(
                                                  libp2p::rendezvous::client::Event::Discovered { registrations, .. },
                                              )) => {
                            self.state = State::WaitForQuoteCompletion;

                            for registration in registrations {
                                let peer = registration.record.peer_id();
                                for address in registration.record.addresses() {
                                    tracing::info!(peer_id=%peer, address=%address, "Discovered peer");

                                    let address_with_p2p = seller_multiaddr(peer, address);

                                    self.quote_tracker.asb_quote_status.insert(peer, QuoteStatus::Pending);

                                    // add all external addresses of that peer to the swarm
                                    self.swarm.add_peer_address(peer, address_with_p2p);
                                }

                                // request the quote, if we are not connected to the peer it will be dialed automatically
                                let _request_id = self.swarm.behaviour_mut().quote.send_request(&peer, ());
                            }
                        }
                        SwarmEvent::Behaviour(OutEvent::Quote(quote_response)) => {
                            match quote_response {
                                request_response::Event::Message { peer, message } => {
                                    match message {
                                        request_response::Message::Response { response, .. } => {
                                            if self.quote_tracker.asb_quote_status.insert(peer, QuoteStatus::Received(Status::Online(response))).is_none() {
                                                tracing::error!(%peer, "Received bid quote from unexpected peer, this record will be removed!");
                                                self.quote_tracker.asb_quote_status.remove(&peer);
                                            }
                                        }
                                        request_response::Message::Request { .. } => unreachable!()
                                    }
                                }
                                request_response::Event::OutboundFailure { peer, error, .. } => {
                                    if Some(peer) == self.rendezvous_peer_id {
                                        tracing::debug!(%peer, "Outbound failure when communicating with rendezvous node: {:#}", error);
                                    } else {
                                        tracing::debug!(%peer, "Ignoring seller, because unable to request quote: {:#}", error);
                                        self.quote_tracker.asb_quote_status.remove(&peer);
                                    }
                                }
                                request_response::Event::InboundFailure { peer, error, .. } => {
                                    if Some(peer) == self.rendezvous_peer_id {
                                        tracing::debug!(%peer, "Inbound failure when communicating with rendezvous node: {:#}", error);
                                    } else {
                                        if proxy_port == 0 {
                                            // When using Tor, we seem to get these inbound failures when requesting quotes from seller... doesn't make much sense, but this is a nice workaround I think (only checking this when not using Tor).
                                            tracing::debug!(%peer, "Ignoring seller, because unable to request quote: {:#}", error);
                                            self.quote_tracker.asb_quote_status.remove(&peer);
                                        }
                                    }
                                },
                                request_response::Event::ResponseSent { .. } => unreachable!()
                            }
                        }
                        _ => {}
                    }
                }
            }

            match self.state {
                State::WaitForDiscovery => {
                    continue;
                }
                State::WaitForQuoteCompletion => {
                    let all_quotes_fetched = self
                        .quote_tracker
                        .asb_quote_status
                        .iter()
                        .map(|(peer_id, quote_status)| match quote_status {
                            QuoteStatus::Pending => Err(StillPending {}),
                            QuoteStatus::Received(Status::Online(quote)) => {
                                let address = self
                                    .quote_tracker
                                    .reachable_asb_address
                                    .get(peer_id)
                                    .expect("if we got a quote we must have stored an address");

                                Ok(Seller {
                                    multiaddr: address.clone(),
                                    status: Status::Online(quote.clone()),
                                })
                            }
                            QuoteStatus::Received(Status::Unreachable) => {
                                let address = self
                                    .quote_tracker
                                    .unreachable_asb_address
                                    .get(peer_id)
                                    .expect("if we got a quote we must have stored an address");

                                Ok(Seller {
                                    multiaddr: address.clone(),
                                    status: Status::Unreachable,
                                })
                            }
                        })
                        .collect::<Result<Vec<_>, _>>();

                    match all_quotes_fetched {
                        Ok(mut sellers) => {
                            sellers.sort_by(|a, b| format!("{}", a.multiaddr).cmp(&format!("{}", b.multiaddr)));
                            break sellers;
                        }
                        Err(StillPending {}) => continue,
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
struct StillPending {}

impl From<ping::Event> for OutEvent {
    fn from(event: ping::Event) -> Self {
        OutEvent::Ping(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swap::network::quote::RefundPolicyWire;
    use std::collections::HashMap;

    fn online_quote() -> BidQuote {
        BidQuote {
            price: Default::default(),
            min_quantity: Default::default(),
            max_quantity: Default::default(),
            refund_policy: RefundPolicyWire::FullRefund,
            reserve_proof: None,
        }
    }

    #[test]
    fn seller_addresses_always_include_the_peer_id() {
        let peer_id: PeerId = "12D3KooWEMq58eksT5eJPookK5SGVkXaMSYuNMHdorKRoWYLyui8"
            .parse()
            .unwrap();
        let address = "/onion3/ubiamiotqmokfiidjingnx3cdlzyeo2tq4kbqoba5k6oqotknstfrgyd:9939"
            .parse::<Multiaddr>()
            .unwrap();

        let seller = seller_multiaddr(peer_id, &address);
        assert_eq!(seller.extract_peer_id(), Some(peer_id));

        let once = seller_multiaddr(peer_id, &seller);
        assert_eq!(once, seller, "the peer ID suffix must not be duplicated");
    }

    #[test]
    fn deadline_collection_drops_pending_and_reports_settled() {
        // Regression for the stuck-at-N/11 discovery hang: when the hard
        // deadline fires, makers whose quotes never resolved must be dropped
        // and settled ones reported — in BOTH direct and rendezvous mode (the
        // deadline used to exist in direct mode only).
        let mut quote_status = HashMap::new();
        let mut reachable = HashMap::new();
        let mut unreachable = HashMap::new();

        let settled_online: PeerId = "12D3KooWEMq58eksT5eJPookK5SGVkXaMSYuNMHdorKRoWYLyui8"
            .parse()
            .unwrap();
        let settled_unreachable: PeerId = "12D3KooWRWKn8xgiGk5fuHVa11yvG87748jYHxvTVhhLM5VEuw5s"
            .parse()
            .unwrap();
        let pending: PeerId = "12D3KooWGAWDhDe68B89EScNMAR8XNzVSt8uMMWn8pwse6T2U68K"
            .parse()
            .unwrap();

        quote_status.insert(settled_online, QuoteStatus::Received(Status::Online(online_quote())));
        reachable.insert(
            settled_online,
            "/ip4/127.0.0.1/tcp/5678".parse::<Multiaddr>().unwrap(),
        );
        quote_status.insert(
            settled_unreachable,
            QuoteStatus::Received(Status::Unreachable),
        );
        unreachable.insert(
            settled_unreachable,
            "/ip4/127.0.0.1/tcp/1234".parse::<Multiaddr>().unwrap(),
        );
        quote_status.insert(pending, QuoteStatus::Pending);

        let tracker = QuoteTracker {
            asb_quote_status: quote_status,
            reachable_asb_address: reachable,
            unreachable_asb_address: unreachable,
        };

        let sellers = tracker.settled_sellers();
        assert_eq!(sellers.len(), 2, "pending maker must be dropped, settled ones reported");
        assert!(sellers
            .iter()
            .all(|s| s.multiaddr != Multiaddr::empty()),
            "settled sellers must carry their dial address");
        assert!(sellers
            .iter()
            .any(|s| matches!(s.status, Status::Online(_))));
        assert!(sellers
            .iter()
            .any(|s| matches!(s.status, Status::Unreachable)));
    }

    #[test]
    fn sellers_sort_with_unreachable_coming_last() {
        let mut list = vec![
            Seller {
                multiaddr: "/ip4/127.0.0.1/tcp/1234".parse().unwrap(),
                status: Status::Unreachable,
            },
            Seller {
                multiaddr: Multiaddr::empty(),
                status: Status::Unreachable,
            },
            Seller {
                multiaddr: "/ip4/127.0.0.1/tcp/5678".parse().unwrap(),
                status: Status::Online(BidQuote {
                    price: Default::default(),
                    min_quantity: Default::default(),
                    max_quantity: Default::default(),
                    refund_policy: RefundPolicyWire::FullRefund,
                    reserve_proof: None,
                }),
            },
        ];

        list.sort();

        assert_eq!(
            list,
            vec![
                Seller {
                    multiaddr: "/ip4/127.0.0.1/tcp/5678".parse().unwrap(),
                    status: Status::Online(BidQuote {
                        price: Default::default(),
                        min_quantity: Default::default(),
                        max_quantity: Default::default(),
                        refund_policy: RefundPolicyWire::FullRefund,
                        reserve_proof: None,
                    })
                },
                Seller {
                    multiaddr: Multiaddr::empty(),
                    status: Status::Unreachable
                },
                Seller {
                    multiaddr: "/ip4/127.0.0.1/tcp/1234".parse().unwrap(),
                    status: Status::Unreachable
                },
            ]
        )
    }
}

#[cfg(test)]
mod direct_tests {
    use super::*;
    use swap::network::quote::RefundPolicyWire;
    use swap::seed::Seed;

    // LIVE-network test: dials the curated known-makers directly through the
    // local Tor SOCKS proxy. Run explicitly: cargo test -- --ignored
    #[tokio::test]
    #[ignore = "live network test: requires a Tor SOCKS proxy on 127.0.0.1:9155"]
    async fn direct_known_makers_return_quotes_without_rendezvous() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
        let seed = Seed::from([7u8; 64]);
        let identity = seed.derive_libp2p_identity();
        let maker_addresses: Vec<Multiaddr> = [
            "/dns4/xmr.swapexchange.club/tcp/443/wss/p2p/12D3KooWP8M6ororMZkCTyEKoY2m6MTwX8SHm7NgAcwFqKnFPPbR",
            "/onion3/u7akjfnrz4bjlvole5erjiok6wysthz6xgntxglvx2m7br3663wvyuqd:9939/p2p/12D3KooWRfthPEkt7TcbG1oyrfg2mTQ2oDHxUiipMUwGHdS7JFec",
            "/onion3/geypac53etgwloptq36rjbtccrahji42vrupeqnfdjo3f42gpmxx44yd:9939/p2p/12D3KooWJ1xBqcvVTJQMcvhNvrsLvPS2gT3jCGBW5cX4x2tssWvt",
            "/onion3/xcprxnhtix4zegs3uej43pxbr67wmi43ixe4m5itism52t6fyxdzx4yd:9939/p2p/12D3KooWA4fNnffnwecJikZBBZecKvupA6p1EdMTSvuD5f9G8NPe",
            "/dns4/whaleswap.club/tcp/443/wss/p2p/12D3KooWNTqfRudnVnTrScS5urcoq5gGbjKfJWXTWzymW4FpfPkL",
            "/dns4/untraceable.exchange/tcp/443/wss/p2p/12D3KooWAh7Su84tbFCQfQumZkp1zDwwmu1VJDoUXKnca68B8sLu",
            "/dns4/p2p.btc-xmr.com/tcp/9941/p2p/12D3KooWAFX9kzkhks54mSj6zwWkvcFd5KkHMkBRTb7f3uvNv1uL",
        ]
        .iter()
        .map(|address| address.parse().unwrap())
        .collect();

        let sellers = internal_list_sellers_direct(maker_addresses, 9155, identity)
            .await
            .expect("direct seller listing failed");

        let online: Vec<_> = sellers
            .iter()
            .filter(|seller| matches!(seller.status, Status::Online(_)))
            .collect();
        println!("direct dial: {} sellers, {} online", sellers.len(), online.len());
        for seller in &online {
            if let Status::Online(quote) = &seller.status {
                println!("ONLINE {} @ {}", seller.multiaddr, quote.price);
            }
        }
        assert!(
            !online.is_empty(),
            "expected at least one online maker via direct dial while rendezvous points are down"
        );
    }
}

#[cfg(test)]
mod log_level_tests {
    /// Unreachable sellers are routine over Tor: they must not be logged as errors (they filled
    /// a finished swap's log with dozens of ERROR lines). Source check: the swarm loop needs Tor.
    #[test]
    fn unreachable_sellers_are_not_errors() {
        let source = include_str!("internal_list_sellers.rs");
        let source = &source[..source.find("mod log_level_tests").unwrap()];
        let at = source.find("\"Failed to connect to peer: {}\"").unwrap();
        let before = &source[..at];
        assert!(before.rfind("tracing::debug!(").unwrap() > before.rfind("tracing::error!(").unwrap_or(0));
        let rendezvous = source.find("\"Failed to connect to rendezvous point: {}\"").unwrap();
        assert!(source[..rendezvous].rfind("tracing::warn!(").is_some());
        assert!(!source.contains("println!(\"Failed to connect"));
    }
}
