# Local `bdk_chain` patch

Source: crates.io `bdk_chain 0.23.4`.

Change: raise the optional `rusqlite` dependency from `0.31.0` to `0.32.0`.

Reason: `bdk_chain 0.23.4`'s default `rusqlite 0.31` links
`libsqlite3-sys 0.28`, while the application's `sqlx 0.8.6` links
`libsqlite3-sys 0.30.1`. Cargo forbids both. `rusqlite 0.32` links the
same `libsqlite3-sys 0.30` line as sqlx 0.8.6. The only alternative was
pinning `sqlx 0.8.0`, which is affected by RUSTSEC-2024-0363.

No Rust source changes were made. Remove this patch once BDK publishes a
`bdk_chain` release that depends on rusqlite 0.32 or newer.
