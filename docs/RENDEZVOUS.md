# Running a Rendezvous Point

Outpost finds sellers (market makers) through **rendezvous points**: community-run "phone book" servers that ASBs register with and takers query for the seller list. Rendezvous points are plain libp2p servers — they are **not** part of the swap itself, never see swap details, and hold no funds. Once a taker and a maker find each other, the swap is direct peer-to-peer over Tor.

The app's localhost rendezvous override is for development and testing only; release builds do not include a localhost peer. This guide is about running a separate server, which the app does not bundle or start.

## Contents

- [Why run one?](#why-run-one)
- [Build the vendored server](#build-the-vendored-server)
- [Run it](#run-it)
- [Publish it over Tor](#publish-it-over-tor)
- [Submit your multiaddr](#submit-your-multiaddr)

## Documentation

- [Buying XMR](BUY_XMR.md)
- [Selling XMR](SELL_XMR.md)
- [Withdrawing BTC](WITHDRAW.md)
- [Fees](FEES.md)
- [Settings](SETTINGS.md)
- [Swap architecture](ATOMIC_SWAPS.md)
- [Privacy and network routing](PRIVACY.md)
- [Security model](SECURITY.md)
- [FAQ](FAQ.md)

## Why run one?

Maker discovery primarily uses rendezvous points; the wallet also quotes a small curated known-makers list directly, so a total rendezvous outage degrades the seller list instead of emptying it. Running a rendezvous point restores full discovery for everyone. All of them going down at once leaves every wallet in the network unable to list sellers until one returns. Every extra operator is one more independent phone book: more uptime for everyone, and less concentration on a handful of volunteers.

If you run public infrastructure already (a node operator, a pool, a community server), a rendezvous point is cheap to operate: it is a single small binary with no database, no secrets beyond its own key file, and negligible bandwidth.

## Build the vendored server

The server source is vendored in this repository at [`xmr-btc-swap/swap/src/bin/rendezvous_node.rs`](../xmr-btc-swap/swap/src/bin/rendezvous_node.rs). It uses the same libp2p stack as the wallet and serves all namespaces, so one node can handle mainnet and testnet registrations.

From the `xmr-btc-swap/` directory:

```
cargo build --release -p swap --bin rendezvous_node
```

## Run it

```
./target/release/rendezvous_node 8888 /path/to/rendezvous-server-secret.key
```

Arguments are positional:

1. **Port** — optional, defaults to `8888`.
2. **Secret file** — optional, defaults to `rendezvous-server-secret.key` in the current directory.

The server listens on plain TCP (`/ip4/0.0.0.0/tcp/<port>`) and prints its listening address and peer ID. If the secret file is missing it generates a new 32-byte key and writes it there.

Keep the secret file backed up: it determines the peer ID. The binary does **not** start Tor or create an onion service by itself.

## Publish it over Tor

For public use, put the TCP listener behind a Tor onion service. For example, point a hidden service at `127.0.0.1:8888`, then use the resulting onion address in the multiaddr.

Two keys must survive restarts:

- The server secret file determines the **peer ID**.
- The Tor hidden-service directory determines the **onion address**.

Back up both, or your published multiaddr changes. Run it under a supervisor such as systemd and keep it online.

## Submit your multiaddr

Open an issue or pull request against this repository so the default list can be updated. The multiaddr looks like:

- Onion TCP: `/onion3/<56-char-address>.onion:8888/p2p/<peer-id>`
- Clearnet WSS (only if you run a separate websocket front end): `/dns4/your.domain/tcp/443/wss/p2p/<peer-id>`

The vendored server itself provides plain TCP, not WSS. The current default list lives in [`app/src/main/java/swap/lib/AppSwap.java`](../app/src/main/java/swap/lib/AppSwap.java); healthy additions and confirmed-dead entries are reviewed over time.
