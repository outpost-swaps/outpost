# Import delta

Source: `eigen-core/swap-machine/src/common/mod.rs`, commit `cc1de7f3690b29605e14dd45b897347881050f2f` (5.0.2).

Imported: setup messages `Message0`-`Message4`, the cross-curve DLEQ proof system,
ownership challenge, fee-floor validation, fee limits, and swap-fee calculation.

Omitted: the `State` enum, `AliceState`/`BobState` conversions, and the `Database`
trait. Those require the Monero C++ wallet stack (`monero-sys`), whose submodules
are empty in the reference checkout. They will be imported with `swap-machine`
once the Monero reference build is available.
