# Outpost swap engine

This folder holds the Rust swap engine and the ASB (the seller side) that Outpost uses to swap
Bitcoin for Monero. It is built into the app through the JNI library in the repository root
(`src/`); you normally build it with `scripts/prepare/` or `./start.sh`, not on its own.

## Where it comes from

It is a derived work, not the upstream project:

- It started from [COMIT's `xmr-btc-swap`](https://github.com/comit-network/xmr-btc-swap), version 0.12.3,
  under the GNU GPL v3, and has been changed a great deal since.
- Swap protocol 5 (`swap-setup` 5.0.0) follows [Eigenwallet](https://github.com/eigenwallet/core) 5.0.2.
  `protocol-v5/` holds that protocol's crates, and `bitcoin-wallet`, `electrum-pool` and
  `utxo-ownership-proof` were taken from Eigenwallet and adapted.
- See `NOTICE` in the repository root for the full credits.

Do not use the upstream projects' binaries, release pages or support channels for Outpost. Report
problems for this engine in this repository.

## Background on the protocol

It implements the protocol in section 3 of [this paper](https://arxiv.org/abs/2101.12332). More
information in this [presentation](https://youtu.be/Jj8rd4WOEy0) and these COMIT posts:
[Monero-Bitcoin atomic swaps](https://comit.network/blog/2020/10/06/monero-bitcoin/) and
[transaction presigning](https://comit.network/blog/2021/07/02/transaction-presigning).

## Working on it

- `scripts/ci/check.sh` runs the tests (see `CONTRIBUTING.md` for the rules for changes).
- The Rust toolchain is pinned in `rust-toolchain.toml`.
- Swap-safety rules (timelocks, refund, redeem) are in `CONTRIBUTING.md`: read them before touching those paths.

## License

GNU General Public License v3. See `LICENSE`.
