# Eigenwallet 5.0.2 reference import

Source: `https://github.com/eigenwallet/core`
Commit: `cc1de7f3690b29605e14dd45b897347881050f2f` (`Prepare release 5.0.2 (#1247)`)
Imported: 2026-11-01

## Imported crates

- `swap-core` (transaction primitives: lock, cancel, refund, redeem, punish,
  early refund, reclaim, withhold, mercy, partial refund, fees, timelocks)
- `swap-machine-common` (subset of `swap-machine/src/common/mod.rs`: setup
  messages 0-4, DLEQ proof system, ownership challenge, fee floor/limits)
- `swap-serde`
- `swap-env`
- `swap-fs`
- `swap-proptest`
- `monero-oxide-ext`
- `tracing-ext`
- `swap-feed`
- `swap-machine`
- `swap-p2p`
- `swap-db`
- `conformance` (local crate, not upstream: frozen setup-message and fee vectors)
- `outpost-monero-wallet` (local crate, not upstream: callback-based `Wallets`
  shim for the single `has_received_outputs` call in `swap-machine`)

## Deliberate deltas

- `monero-oxide` dependencies are pinned to the reference lockfile revision
  `c8be5d3d1287669946a83fbfcb296ce2a8852e47`. Resolving the git branch head
  silently changes serialization/overflow behavior.
- `swap-machine-common` omits the upstream `State` enum, `AliceState`/`BobState`
  conversions, and `Database` trait.
- `swap-machine` now depends on the local `outpost-monero-wallet` shim instead of
  Eigenwallet's `monero-wallet`. The upstream crate only uses
  `Wallets::has_received_outputs` (`swap-machine/src/alice/mod.rs`), so the shim
  exposes that one operation as a callback trait. No `monero-sys` dependency and
  no Monero C++ submodule build are required.
- `swap-p2p`'s `fetch_quotes` example and its Arti/`libp2p-tor` dev-dependencies
  were removed; the library itself is unchanged.
- The protocol engine reuses Outpost's real `bitcoin-wallet`, `electrum-pool`,
  and `utxo-ownership-proof` packages via `../../` path dependencies. The
  duplicate copies imported during isolation were removed, so the engine and
  the app share one wallet implementation (Outpost account derivation, Dojo
  fees/broadcast, and transaction policy included). `swap-machine` only depends
  on the `BitcoinWallet` trait, so the substitution is source-compatible.
- The app links `swap-machine` and `swap-p2p` through the optional
  `protocol-v5` feature (`swap/src/protocol_v5.rs`); live swaps remain on the
  legacy module. The main workspace excludes `protocol-v5`, and
  `protocol_v5::as_protocol_wallet` borrows Outpost's wallet as the engine's
  `BitcoinWallet` trait object.
- The unused direct `monero-oxide` dependency was removed from `swap-machine`.

## Verification

- `cargo test -p swap-core`: 90 passed, 1 failed.
  The failure is upstream: `monero::primitives::tests::parse_monero_overflows`
  expects `swap_core::monero::OverflowError`, but `monero-oxide-ext` returns
  `anyhow!("too big")`. Recorded, not patched.
- `cargo test -p protocol-v5-conformance`: 4 passed.
  Freezes `Message0`/`Message1` JSON and CBOR hashes, the 5.0.2 setup fee
  limits (`rate 5000 -> min 2500, max 15000, max path fee 100000`), and the
  lock/cancel/full-refund/redeem/punish/partial-refund/early-refund/reclaim
  transaction IDs generated from fixed keys and amounts.
- `cargo test --workspace --exclude swap-core --lib`: 100 passed
  (includes swap-machine 11, swap-p2p 45, swap-feed 31, swap-db 2; the 35
  wallet-crate tests now run from the main workspace).
- `cargo check --workspace`: passed; `Cargo.lock` contains no `monero-sys`.
- From `xmr-btc-swap`: `cargo check -p swap --features protocol-v5` passed;
  swap lib tests 222 passed / 1 ignored normally and 223 / 1 with the feature;
  Outpost `bitcoin-wallet` 43 passed.
- The feature test runs the full Bob setup in memory with Outpost's real test
  wallet, persists the canonical `swap-db` state, reloads it, and re-runs
  `lock_btc()` from the recovered state. A `swap-p2p` test additionally runs
  the whole setup over a real in-memory substream with the production
  `LockInputVerifier` and `bmrng` channels. A second `swap-p2p` test drives the
  real Alice/Bob behaviours over libp2p's memory transport through the commit
  decision and lock-input reservation.
- The remaining `bitcoin-wallet` integration tests require Docker/bitcoind.
- The `jsonrpc_client` patch was removed after the duplicate wallet crates were
  deleted; it is no longer needed by this workspace.

## Commands

From `xmr-btc-swap/protocol-v5`:

```sh
cargo test -p swap-core
cargo test -p protocol-v5-conformance
cargo test -p swap-machine --lib
cargo test -p swap-p2p --lib
cargo test -p swap-feed --lib
cargo check --workspace
```
