# Protocol v5 redeem/cancel state flow

This documents the legacy state/UI behavior and the v5 invariants that prevent
premature cancel/refund or refunded UI states.

## Legacy flow

| Persisted state | On-chain meaning | UI event/status |
|---|---|---|
| `BtcLocked` | Bitcoin lock exists, waiting for Alice | BTC locked |
| `XmrLocked` | Alice's Monero lock proof accepted | XMR locked |
| `EncSigSent` | Redeem signature sent to Alice | XMR locked / redeem in progress |
| `CancelTimelockExpired` | Lock timelock is actually mature | Cancel timelock expired |
| `BtcCancelled` | Cancel transaction submitted | `onSwapCanceled`; cancelled |
| `BtcRefunded` | Refund published/confirmed | `onSwapRefunded`; refunded |

The legacy client only calls `submit_tx_cancel()` and
`on_swap_canceled()` from `CancelTimelockExpired`, i.e. after the lock's cancel
timelock has genuinely expired (`src/swap_state_manager.rs`). It keeps waiting
in `XmrLocked` while the redeem exchange can still complete.

## v5 regression

`src/buy_xmr_v5.rs` previously called `cancel_lock()` on **any**
`run_until_signature_sent` error. That persisted `BtcCancelled` and started the
cancel/refund loop before the lock timelock, so the UI and history moved to
cancel/refund while the swap was still BTC/XMR locked.

The ASB (`xmr-btc-swap/swap/src/protocol_v5/alice.rs`) also abandoned the
transfer proof after one failed acknowledgement and waited out the cancel
timelock, so Bob could never receive the proof and redeem.

## Fixed v5 flow

1. Bob stays in the persisted locked state while the redeem exchange can still
   complete.
2. If the redeem exchange returns an error, Bob checks the lock's actual
   cancel timelock:
   - not expired -> retry the redeem exchange after a short delay;
   - expired -> only then persist the cancel path and run cancel/refund.
3. The ASB retries the transfer proof/acknowledgement every 10 seconds while
   the cancel timelock has not expired. The proof remains valid until then.
4. UI/history are derived from the persisted state. `BTC locked` / `XMR locked`
   must remain until the real cancel transition; `Refund pending` starts only
   after the cancel/refund path is real, and `Swap refunded` only after the
   refund confirms.

## Early refund, cancel/refund and punish (2026-11-10)

Changes from the 2026-11-10 security audit (findings H-2, H-3, H-5; matrix rows
S-31, S-32, S-33). Behavior matches Eigenwallet's `swap/src/protocol/bob/swap.rs`
unless noted.

- **Early refund (H-3).** Alice's `TxEarlyRefund` ends the swap only once it is
  confirmed (`V5_BOB_EARLY_REFUND_CONFIRMED`), not when it is first seen. A
  persisted `BtcEarlyRefundPublished` races that confirmation against the cancel
  timelock; if the timelock wins, Bob records `CancelTimelockExpired` (not
  `BtcCancelled`: nothing is cancelled yet) and runs cancel/refund.
- **Cancel/refund watches the early refund.** Every round of
  `run_cancel_and_refund` reads the early refund first: confirmed means
  `Done(BtcEarlyRefunded)` and no cancel; in the mempool means no cancel is
  published (it would only conflict) and the loop waits; unseen means cancel and
  refund as before. Upstream does the same in `CancelTimelockExpired` and
  `BtcCancelPublished`. Without it, an early refund that confirmed after the
  timelock left the cancel retrying "inputs spent" forever.
- **Punish window (H-5).** At `ExpiredTimelocks::Punish` Bob records
  `BtcPunished` only when the punish transaction is confirmed; otherwise he keeps
  publishing the refund. Upstream records it as soon as the punish transaction
  exists and then asks Alice for a cooperative Monero redeem; Outpost waits for
  the confirmation and does not implement the cooperative redeem.
- **Partial refunds (seller deposit; audit H-1).** A seller with an anti-spam
  deposit signs only a partial refund. `publish_refund` records
  `BtcPartialRefundPublished`; once it confirms, `BtcPartiallyRefunded`, then
  `reclaim_deposit` (one decision, `deposit_step`, over upstream's
  `WaitingForReclaimTimelockExpiration` / `ReclaimTimelockExpired` /
  `BtcReclaimPublished` / `BtcWithholdPublished` / `BtcWithheld` /
  `BtcMercyPublished`): after `remaining_refund_timelock` confirmations it
  publishes `TxReclaim` (Alice's amnesty signature from setup); a withhold in the
  mempool holds the reclaim back; a confirmed withhold ends it as withheld unless
  a mercy follows. Ends as `Done(BtcReclaimConfirmed)` / `Done(BtcMercyConfirmed)`
  (shown as refunded) or `BtcWithheld` (history `PARTIALLY_REFUNDED`,
  `onSwapPartiallyRefunded`). Unlike upstream, a confirmed withhold also ends a
  reclaim wait. Before this, a partial refund was stored as `Done(BtcRefunded)`
  and shown as refunded, and the deposit was never claimed.
- **Errors after the lock (H-2).** Timelock reads in the cancel/refund loop retry
  with backoff until a deadline. Every failure after the lock may be published is
  reported as `SwapInterrupted`, which the GUI answers by resuming the same swap,
  never by starting a new one.

## UI/mock state mapping

The Option+Arrow mock (`SwapUiDemo`) and the real UI must share these states:

| UI/mock state | Persisted Bob v5 state | Display |
|---|---|---|
| 5 | `BtcLocked` (unconfirmed) | step 2, `confirming`, BTC lock icon |
| 6 | `BtcLocked` at finality / healthy lock-confirmed wait | step 2, `confirmed`, “BTC lock confirmed - waiting for Alice to lock XMR” |
| 7 | `XmrLocked` / proof received | step 3, XMR locked |
| 9 (10/10) | `XmrLocked` at Monero finality, waiting for Alice's redeem | step 3 `confirmed`, step 4 current `waiting for seller` |
| 10 | `BtcRedeemed` / redeem sweep | step 4 `claimed`, step 5 `preparing` → `sweeping` |
| 14 (success) | `XmrRedeemed` | all steps done (`sent`, `confirmed`, `confirmed`, `claimed`, `swept`); exact XMR received and sweep fee in the progress bar's slot when reported |
| new refund-waiting | `BtcCancelled` / `CancelTimelockExpired` before the cancel transaction exists | step 2, `confirmed`, **Refund pending**, BTC lock icon, no canceled mark, no tx claim |
| 15 | `BtcCancelPublished` / refund published | step 2 canceled, “Refund pending”, cancel/refund tx exists |

The v5 regression was putting the pre-cancel `BtcCancelled` state into state 5/6
without a separate mock/UI state. That hybrid — “BTC locked” plus refund details
on the lock step — is the limbo. The explicit refund-waiting state above is the
missing state; the healthy path must stay in states 6 → 7 → 10.

## Swap Success display (2026-11-11, matrix N-15)

- Step 4 (`BTC redeemed`) becomes current at Monero finality with `waiting for
  seller` and reads `claimed` once Alice's redeem is seen (`SwapStepLabels`).
- The success screen shows the exact XMR received and the sweep's network fee.
  Received = the agreed amount (the lock is accepted only with an output of
  exactly that amount) minus the sweep's public RingCT fee
  (`ConstructedSweep::fee_piconero`, `bob::xmr_received`). Both daemon sweep
  paths, including a stored sweep sent again on resume, report it through
  `onXmrReceived` (`([BJJ)V`); without a report no number is shown.

## Invariants

- No `cancel_lock()` before `ExpiredTimelocks::cancel_timelock_expired()`.
- No `onSwapCanceled` before the cancel transaction is accepted for broadcast.
- No `onSwapRefunded` before the refund transaction is confirmed.
- No UI “cancelled”/“refunded” headline while the persisted state is still
  BTC/XMR locked.
- No early refund counted before it is confirmed; no cancel published while an
  early refund is in the mempool.
- No `BtcPunished` before the punish transaction is confirmed.
- No new swap started in place of a swap whose Bitcoin may be locked.
- No "refunded" for a partial refund until the deposit is back; a kept deposit is
  "partially refunded".
- No reclaim published while the seller's withhold is in the mempool.
- No received amount shown that the engine did not report for this swap; never the quote.

## Tests

- `src/buy_xmr_v5.rs` `redeem_failure_policy_tests`: a missing timelock expiry
  returns `RetryRedeem`; `Cancel`/`Punish` timelocks allow `CancelRefund`.
- `app/src/test/java/swap/listener/impl/SwapListenerRefundUiTest`: the
  pre-timelock headline is `BTC locked` and the log says no transaction was
  broadcast.
- `app/src/test/java/swap/gui/controller/pages/HistoryControllerStatusTest`:
  `REFUND_PENDING` is not rendered as `Refunded`/`Cancelled`.
- `xmr-btc-swap/swap/src/protocol_v5/bob.rs` `early_refund_tests`,
  `early_refund_check_tests`, `punish_window_tests`, `timelock_retry_tests`;
  `protocol_v5/tests.rs` `cancel_path_early_refund` (the real cancel/refund loop
  against a scripted chain: a confirmed early refund ends it without a cancel, one
  in the mempool holds the cancel back).
- `bob.rs` `deposit_step_tests`; `protocol_v5/tests.rs` `partial_refund_deposit`
  (real `reclaim_deposit` / `finish_refund` / `publish_refund` against a
  partial-refund State6 with a 2% deposit, a SQLite store and a scripted chain).
- `bob.rs` `xmr_received_tests`; `src/util.rs` `xmr_received_report_tests`; Java
  `SwapReceivedTest`, `SwapStepLabelsTest` (step 4 waits, then `claimed`).
- `src/util.rs` `refund_report_tests`, `post_lock_error_tests`, `cancel_refund_error_tests`;
  Java `HistoryControllerStatusTest`, `SwapListenerPartialRefundTest`;
  `app/src/test/java/swap/model/SwapErrorTypeTest`.
- `xmr-btc-swap/swap/tests/protocol_v5_happy_path.rs`: passing end-to-end
  regtest test: lock -> XMR lock proof/ack -> redeem.
- `xmr-btc-swap/swap/tests/protocol_v5_asb_process.rs`: now compiles and has the
  refund state/event assertions, but its separate ASB child-process harness
  currently fails before any swap at the quote/dial setup; this is tracked as a
  harness issue, not a state-machine result.
