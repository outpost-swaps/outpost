# Contributing to Outpost

Outpost is a desktop app for BTC ↔ XMR atomic swaps. It moves real money, so changes are held
to a higher bar than usual: understand the swap path you touch, test it, and keep users' funds
and privacy safe.

## Layout

- `app/`: JavaFX GUI (Gradle; entry point `swap.gui.Main`).
- `src/`: the Rust JNI library (`atomicswap`) the GUI calls.
- `xmr-btc-swap/`: the Rust swap engine and ASB (seller), derived from COMIT's `xmr-btc-swap`.
  `xmr-btc-swap/protocol-v5/` holds the swap protocol shared with Eigenwallet 5.x.
- `libs/darkjar/`: vendored Java wallet libraries (Samourai extlibj, a bitcoinj fork, Sparrow nightjar).
- `scripts/`: native build (`prepare/`), packaging (`package/`), CI (`ci/check.sh`) and node-list maintenance.
- `docs/`: user documentation. Keep it accurate when behaviour changes.

## Build and run

- JDK 25 and Rust (the toolchain is pinned in `rust-toolchain.toml`). Use `./gradlew`, not a system Gradle.
- `./start.sh` (or `start.bat`) rebuilds the native parts and launches the app. `SKIP_PREPARE=1 ./start.sh`
  skips the native rebuild.
- `scripts/prepare/prepare-<os>` builds the ASB, price ticker, daemon-pool forwarder and JNI library and
  copies them into the app resources with a build manifest.
- Releases are built by `.github/workflows/release.yml` on a `v*` tag.

## Tests

- Run `scripts/ci/check.sh` before proposing a change. While the app is running from the same tree,
  use `scripts/ci/check.sh --java-copy <dir>`: rebuilding the jar under a running app breaks it.
- Every change needs a test that fails without it. For bug fixes, first write the test that reproduces
  the bug.
- Protocol and fee changes must keep the cross-client conformance tests passing
  (`xmr-btc-swap/protocol-v5/conformance/`): Outpost has to stay compatible with Eigenwallet peers.
  Fees are compiled into each client, not negotiated, so a local change breaks signatures with other clients.

## Rules for changes

- **Swap safety first.** Anything after the Bitcoin lock (cancel, refund, punish, redeem, sweep) can cost
  users money. Do not change those paths without understanding the timelocks and adding tests.
- **Tor only.** Every network connection goes through Tor. Never add a direct clearnet connection or a
  clearnet fallback.
- **No secrets on disk in plaintext.** Seeds, passphrases and Dojo API keys stay encrypted at rest; keep
  them out of logs, files and process environments.
- **Keep docs in step.** User-visible behaviour changes update `docs/`.
- Do not commit build outputs (native libraries, packaged apps, `target/`, `dist/`).
- Do not rewrite history or force-push shared branches.
