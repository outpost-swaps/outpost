# jsonrpc_client (Outpost fork)

Vendored from `jsonrpc_client` 0.7.1 to support the workspace's `reqwest 0.12`
upgrade. The upstream crate's reqwest backend is pinned to `reqwest 0.11`.

Two backend features are provided:

- `reqwest` — the original 0.11 backend, retained for the `bitcoin-harness`
  test dependency.
- `reqwest-012` — the 0.12 backend used by `monero-rpc`.

License: MIT OR Apache-2.0 (same as upstream).
