# Contribution guidelines

Thank you for wanting to contribute to this project!

## Contributing code

There are a couple of things we are going to look out for in PRs and knowing them upfront is going to reduce the number of times we will be going back and forth, making things more efficient.

1. We have CI checks in place that validate formatting and code style.
   Make sure `dprint check` and `cargo clippy` both finish without any warnings or errors.
   If you don't already have it installed, you can obtain in [various ways](https://dprint.dev/install/).
2. All text document (`CHANGELOG.md`, `README.md`, etc) should follow the [semantic linebreaks](https://sembr.org/) specification.
3. We strive for atomic commits with good commit messages.
   As an inspiration, read [this](https://chris.beams.io/posts/git-commit/) blogpost.
   An atomic commit is a cohesive diff with formatting checks, linter and build passing.
   Ideally, all tests are passing as well but we acknowledge that this is not always possible depending on the change you are making.
4. If you are making any user visible changes, include a changelog entry.

## Contributing issues

When contributing a feature request, please focus on your _problem_ as much as possible.
It is okay to include ideas on how the feature should be implemented but they should be 2nd nature of your request.

For more loosely-defined problems and ideas, consider starting a [discussion](https://github.com/comit-network/xmr-btc-swap/discussions/new) instead of opening an issue.

## Outpost engine rules

This is Outpost's Rust swap engine and ASB. See the repository's top-level `CONTRIBUTING.md` first.

- `protocol-v5/` is the swap protocol shared with Eigenwallet 5.x. Fee derivation, presigned
  transactions and setup messages must stay identical to Eigenwallet's: run
  `cargo test -p protocol-v5-conformance` (from `protocol-v5/`) after any change there.
- `swap/` holds the wallet, ASB and buyer flows. Run `cargo test -p swap --lib --features protocol-v5`.
- `bitcoin-wallet/` handles Electrum, Dojo and confirmation proofs: a transaction counts as confirmed
  only with a merkle proof against a header with valid proof of work. Do not weaken that check.
- Integration tests under `swap/tests/` start Bitcoin, Electrum and Monero containers and need Docker.
- Every fix needs a test that fails without it. Tor-only networking and fail-closed fee estimation are
  deliberate; do not add clearnet or silent fallbacks.
