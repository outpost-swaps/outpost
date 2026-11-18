# Eigenwallet 5.0.2 fee vectors

Golden values computed by **unmodified** Eigenwallet 5.0.2 (`eigenwallet/core` commit
`cc1de7f3690b29605e14dd45b897347881050f2f`), for the cross-client test
`tests/eigenwallet_fee_vectors.rs` and `swap::bitcoin::wallet` tests (matrix S-01, WORKLOG §461).

- `fee-vectors.json`: the presigned-transaction fee schedule (`SwapFeeInputs::calculate`), the
  buyer's acceptance limits (`swap_fee_limits`) and the maker's lock-fee floor
  (`sanity_check_transaction_fee_floor`), over a grid of rates, amounts, deposits and redeem multipliers.
- `generator/`: the program that produced it. It is **not** built in this workspace. To regenerate,
  check out that Eigenwallet commit, copy `generator/` in as a workspace member named
  `conformance-vectors`, and run `cargo run -q -p conformance-vectors > fee-vectors.json`.
  The generator refuses to run if its copy of `swap_fee_limits` no longer matches their source.
