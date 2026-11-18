# Privacy and network routing

## Contents

- [Short version](#short-version)
- [Current routing](#current-routing)
- [Modes](#modes)
- [What "no clearnet leak" means](#what-no-clearnet-leak-means)
- [Reporting a routing bypass](#reporting-a-routing-bypass)

## Documentation

- [Buying XMR](BUY_XMR.md)
- [Selling XMR](SELL_XMR.md)
- [Withdrawing BTC](WITHDRAW.md)
- [Fees](FEES.md)
- [Settings](SETTINGS.md)
- [Swap architecture](ATOMIC_SWAPS.md)
- [Security model](SECURITY.md)
- [Rendezvous points](RENDEZVOUS.md)
- [FAQ](FAQ.md)

## Short version

Outpost requires Tor and has no Tor-off mode. It does not make direct clearnet connections.

"Tor-routed" is not the same as "onion-only." Some curated fallback servers are clearnet addresses reached **through Tor**; they are used only if you enable Electrum or Daemon Failover in [Settings](SETTINGS.md) (both are off by default, so only the server or daemon you selected is used). They require a Tor exit node, which can see the destination server. Onion services do not require an exit node.

## Current routing

| Traffic | Route |
|---|---|
| Dojo (wallet API, UTXOs, fees, broadcast) | Configured Dojo URL through Tor; public Dojo options are `.onion` |
| Electrum (mainnet) | Your selected server only. With Electrum Failover on: `.onion` first, then clearnet TLS through the local Tor forwarder |
| Electrum (testnet/stagenet) | Your selected server only. With Electrum Failover on: onion pool through Tor |
| Monero daemons | Configured clearnet and `.onion` daemons through Tor |
| Rendezvous and swap traffic | `.onion` and clearnet libp2p peers through Tor |
| Monero wallet RPC | Local loopback process, not a remote connection |
| Price sources | MoneroPriceNow HTTPS through Tor; Feather `.onion` WebSocket fallback; Kraken REST through Tor |

There is no direct clearnet dial for the configured pools. If a connection bypasses Tor, that is a bug and should be reported.

## Modes

**Current build:** Tor is mandatory. Onion endpoints are used when available, and clearnet entries in the configured pools are reached through Tor.

**Under consideration — onion-only mode:** only `.onion` Electrum, Monero, rendezvous, and price sources. Clearnet custom nodes would be rejected. This would avoid exit-node destination exposure, but reduce the number of fallback servers; if onion services were unavailable, the app would fail with an explanation instead of silently using clearnet.

Both modes use Tor. Neither mode permits direct clearnet connections, and neither is a Tor-off mode.

## What "no clearnet leak" means

- No direct DNS, TCP, or TLS connection outside Tor.
- Clearnet entries, when present in a pool, still go through the Tor SOCKS proxy.
- Onion-only mode would additionally avoid exit nodes entirely.
- TLS does not hide the destination from a Tor exit; it only protects the content from the exit.

## Reporting a routing bypass

Direct clearnet DNS, TCP, or TLS connections are not intended. If the app connects outside Tor, treat it as a privacy bug and report it with the relevant log lines; do not include wallet secrets or Dojo API keys.
