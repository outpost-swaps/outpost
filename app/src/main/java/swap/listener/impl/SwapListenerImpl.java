package swap.listener.impl;

import javafx.application.Platform;
import javafx.scene.control.Tooltip;
import javafx.scene.image.Image;
import org.bitcoinj.core.Coin;
import org.bitcoinj.params.MainNetParams;
import swap.gui.GUISwap;
import swap.gui.controller.MainController;
import swap.gui.controller.pages.HistoryController;
import swap.gui.controller.pages.SwapReceived;
import swap.gui.controller.pages.SwapStepLabels;
import swap.gui.controller.pages.SwapsController;
import swap.gui.image.AddressQrCode;
import swap.helper.HelperSwapsDb;
import swap.listener.SwapListener;
import swap.model.*;
import swap.model.response.CompletedSwap;
import swap.model.response.SwapOrder;

import java.util.Locale;
import java.util.Optional;
import java.util.UUID;

// TODO: btcBalance/refundBtcBalance?
// TODO: link tx ids to explorers

public class SwapListenerImpl implements SwapListener {
    static final String STATUS_REFUND_PENDING = "Refund pending";
    static final String STATUS_CANCEL_BROADCAST = "Cancel broadcast";
    static final String STATUS_REFUND_BROADCAST = "Refund broadcast";
    static final String LOG_REFUND_PATH_ARMED =
            "Bitcoin lock is confirmed; refund timelock pending. No cancel or refund transaction has been broadcast yet.";
    static final String LOG_REFUND_WAIT =
            "The client retries each minute. The refund transaction is created only after the lock timelock and the cancel confirmation; no action needed.";

    static final long XMR_FINALITY_CONFIRMATIONS = 10;
    static final String LOG_XMR_FINAL =
            "Monero lock is final. Sending Alice your Bitcoin redeem signature; when she redeems the Bitcoin "
                    + "(a few minutes, once it confirms) your client redeems the Monero.";
    static final String STATUS_XMR_FINAL = "XMR final - waiting for Alice to redeem BTC";

    /** Monero lock transactions whose "final" notice was already shown (updates can repeat). */
    private final java.util.Set<String> finalityAnnounced = java.util.concurrent.ConcurrentHashMap.newKeySet();

    private final MainController mainController;
    private final SwapsController swapsController;
    private final HistoryController historyController; // used for resume swaps

    public SwapListenerImpl(
            MainController mainController,
            SwapsController swapsController,
            HistoryController historyController) {
        this.mainController = mainController;
        this.swapsController = swapsController;
        this.historyController = historyController;
    }

    @Override
    public void printSwapLogLn(String message) {
        mainController.printSwapLogLn(LogType.INFO, message, false);
    }

    public void printSwapLogLn(LogType logType, String message, boolean clearfields, boolean kill) {
        mainController.printSwapLogLn(logType, message, kill);

        if (kill)
            swapsController.swapKillReset(clearfields);
    }

    @Override
    public void onOrderCreated(SwapOrder order) {
        swapsController.stopPreparingHeartbeat();
        // Step 0: Create swap order
        swapsController.updateGui(() -> {
            swapsController.setProgress(-1.0f);
            swapsController.cancelButton.setVisible(true);
            swapsController.cancelButton.setDisable(false);
            swapsController.startButton.setDisable(true);
            swapsController.prepareSwapDisplay("Send between "
                            + Coin.valueOf(order.minimumSatoshis()).toPlainString() + " BTC - "
                            + Coin.valueOf(order.maximumSatoshis()).toPlainString() + " BTC",
                    "waiting for deposit");
            swapsController.setSwapStep(1, "waiting for deposit");
            swapsController.setSwapStatusDetail(order.btcAddress(), "You fully control this address");
        });

        String message =
                "Please send between "
                        + Coin.valueOf(order.minimumSatoshis()).toPlainString() + " BTC - "
                        + Coin.valueOf(order.maximumSatoshis()).toPlainString() + " BTC to: "
                        + order.btcAddress()
                        + " (you control this address)";
        printSwapLogLn(LogType.HIGHLIGHT, message, false, false);
        printSwapLogLn(LogType.WARN, "If you do not meet the minimum deposit amount then the swap will not start.", false, false);

        try {
            Image addressQrCode = new AddressQrCode(order.btcAddress()).generateBtc();
            swapsController.updateGui(() -> {
                swapsController.setSwapImage(addressQrCode, "Send BTC (you control this address)", true);
            });
        } catch (Exception e) {
            System.err.println("Error generating QR Image: " + e.getMessage());
        }

        // The order's maximum is the seller's live advertised max (the quote
        // that governs the swap); reconcile the possibly-stale sellers table.
        swapsController.reconcileSelectedSellerWithLiveQuote(order.maximumSatoshis());
    }

    @Override
    public void onBtcReceived(Coin newBalance, Coin maxGiveable, Coin minQuantity) {
        // Step 0: Send BTC
        swapsController.updateGui(() -> {
            swapsController.setProgress(-1.0f);
            swapsController.cancelButton.setDisable(false);
            swapsController.setSwapStatus("Received Bitcoin - preparing swap...");
            swapsController.setSwapStep(1, "deposit received");
            swapsController.setSwapContextAmount(maxGiveable.value);
        });
        printSwapLogLn("Received Bitcoin, new balance: " + newBalance.toPlainString());

        if (maxGiveable.isLessThan(minQuantity)) {
            Coin difference = minQuantity.subtract(maxGiveable);
            printSwapLogLn("Need ~" + difference.toPlainString() + " more Bitcoin before continuing.");
            printSwapLogLn(LogType.HIGHLIGHT, "Note: Deposited UTXOs will be merged with previously deposited UTXOs when creating the Bitcoin lock transaction.", false, false);
        }
    }

    @Override
    public void onSwapAmountDetermined(long btcSatoshis) {
        swapsController.stopPreparingHeartbeat();
        swapsController.setSwapContextAmount(btcSatoshis);
        swap.model.SellerFeeAllowance.noteSwapAmount(btcSatoshis);
    }

    @Override
    public void onSwapRunning(String swapId, String multiaddr) {
        swapsController.stopPreparingHeartbeat();
        // Step 0: Initiate swap
        swapsController.updateGui(() -> {
            swapsController.setProgress(-1.0f);
            swapsController.setLockIcon(SwapIconType.INITIATED);
            Tooltip.install(swapsController.swapIcon, new Tooltip("Swap Running"));

            swapsController.setSwapStatus("Swap Running");
            swapsController.setSwapStatusDetail(swapId, "Swap ID");
            swapsController.setSwapStep(1, "negotiated");

            swapsController.cancelButton.setDisable(false);
        });

        printSwapLogLn("Swap client started for swap " + swapId + " with peer:");
        printSwapLogLn(multiaddr);
    }

    @Override
    public void onSwapResumeContext(String swapId, String multiaddr, String xmrAddress, long btcSats, long xmrPiconero) {
        // The resume reached a known state: stop "Still resuming..." and let the
        // state callbacks (e.g. onBtcRedeemed) own the status and step labels.
        swapsController.stopResumeHeartbeat();
        swapsController.applyResumedSwapContext(multiaddr, xmrAddress, btcSats, xmrPiconero);
    }

    @Override
    public void onBtcLocked(String btcLockTxid) {
        // The swap state has been reported: resume is over, stop its heartbeat.
        swapsController.stopResumeHeartbeat();
        historyController.refreshHistoryList();
        // Step 1: Lock BTC
        swapsController.updateGui(() -> {
            swapsController.cancelButton.setDisable(false);
            swapsController.startButton.setDisable(true);
            swapsController.setProgress(1f / 15f);

            swapsController.setLockIcon(SwapIconType.BTC_LOCK);
            Tooltip.install(swapsController.swapIcon, new Tooltip("BTC Locked"));

            swapsController.setSwapStatus("BTC locked - waiting on confirmation & Alice's XMR");
            swapsController.setSwapStatusDetail(btcLockTxid, "BTC TX"); // TODO: link to oxt
            swapsController.setSwapStep(1, "sent");
            swapsController.setSwapStep(2, "confirming");
        });

        //HelperSwapsDb.getInstance().setLockTxid(SwapCoin.BTC, "", btcLockTxid); // TODO get swap ID
        int confs = refundTimelockBlocks(GUISwap.appSwap.getParams() == MainNetParams.get());
        if (announcesLockInFull(btcLockTxid)) {
            printSwapLogLn(LogType.HIGHLIGHT, "Locked Bitcoin with Alice in transaction " + btcLockTxid + ".", false, false);
            printSwapLogLn(LogType.WARN, "If Alice fails to lock the Monero, leave the swap client running - you will be refunded when the Bitcoin lock transaction reaches " + confs + " confirmations.", false, false);
        } else {
            // An automatic resume in the same app run: the two lines above were printed already.
            printSwapLogLn(LogType.INFO, resumedLockLine(btcLockTxid), false, false);
        }
        startXmrHeartbeat(
                BTC_LOCK_HEARTBEAT_SECONDS,
                SwapListenerImpl::btcLockWaitStatus,
                SwapListenerImpl::btcLockWaitLog);
    }

    /**
     * Confirmations still to wait, after the first one, until the cancel and refund open up. Protocol 5
     * cancel timelocks: 30 blocks on mainnet, 42 on testnet4 (the engine pins both in
     * protocol_v5::config_tests). The old protocol's 72 and 12 (shown here as 71 and 11 until now) are gone.
     */
    static int refundConfirmationsAfterLock(boolean mainnet) {
        return refundTimelockBlocks(mainnet) - 1;
    }

    /** The protocol 5 cancel timelock in blocks: 30 on mainnet, 42 on testnet4. */
    static int refundTimelockBlocks(boolean mainnet) {
        return mainnet ? 30 : 42;
    }

    @Override
    public void onBtcLockConfirm(String btcLockTxid) {
        stopXmrSyncHeartbeat(); // the "still waiting for the lock to confirm" heartbeat
        swapsController.stopResumeHeartbeat();
        swapsController.updateGui(() -> {
            swapsController.setSwapStatus("BTC lock confirmed - waiting for Alice to lock XMR");
            swapsController.setSwapStep(2, "confirmed");
        });

        int confs = refundConfirmationsAfterLock(GUISwap.appSwap.getParams() == MainNetParams.get());
        printSwapLogLn(LogType.INFO, "Bitcoin lock transaction has confirmed. Waiting for Alice to lock Monero...", false, false);
        printSwapLogLn(LogType.WARN, "If Alice does not respond with the lock proof, please leave the swap running in order to be refunded in " + confs + " confirmations", false, false);
    }

    @Override
    public void onXmrLockProofReceived(String swapId, String xmrLockTxid) {
        stopXmrSyncHeartbeat(); // the Bitcoin-lock wait is over once Alice's proof is here
        swapsController.stopResumeHeartbeat();
        historyController.refreshHistoryList();
        // Step 2: Receive XMR lock proof/XMR has been locked
        swapsController.updateGui(() -> {
            swapsController.cancelButton.setDisable(false);
            swapsController.startButton.setDisable(true);
            swapsController.setProgress(2f / 15f);

            swapsController.setLockIcon(SwapIconType.XMR_LOCK);
            Tooltip.install(swapsController.swapIcon, new Tooltip("XMR lock proof received"));

            swapsController.setSwapStatus("XMR lock proof received - waiting for confirmations");
            swapsController.setSwapStatusDetail(xmrLockTxid, "XMR TX"); // TODO: link to xmr explorer
            swapsController.setSwapStep(3, "awaiting confirmations");
        });

        HelperSwapsDb.getInstance().setLockTxid(SwapCoin.XMR, swapId, xmrLockTxid);
        printSwapLogLn("Received Monero lock proof for transaction " + xmrLockTxid + ". Waiting for confirmations...");
    }

    @Override
    public void onXmrLockConfirmation(String txid, long confirmations) {
        // Step 3-12: XMR Confirmations
        swapsController.stopResumeHeartbeat();
        swapsController.updateGui(() -> {
            swapsController.cancelButton.setDisable(false);
            swapsController.setProgress((Math.min(confirmations, 10L) + 2) / 15f);
            swapsController.setSwapStatus("XMR confirmations: " + confirmations + "/10");
            swapsController.setSwapStep(3, confirmations + "/10 confs");
        });

        printSwapLogLn("Monero lock transaction confirmations: " + confirmations + "/10");

        if (announcesFinality(finalityAnnounced, txid, confirmations)) {
            // Between the last confirmation and "Alice has redeemed the Bitcoin" the client is
            // sending the redeem signature and waiting for her; say so instead of going quiet.
            // The wait is step 4's: it becomes current ("waiting for seller") and step 3 reads
            // "confirmed"; it turns "claimed" in onBtcRedeemed (WORKLOG §455).
            swapsController.updateGui(() -> {
                swapsController.setSwapStatus(STATUS_XMR_FINAL);
                swapsController.setSwapStep(4, SwapStepLabels.WAITING_FOR_SELLER);
            });
            printSwapLogLn(LOG_XMR_FINAL);
        }
    }

    /** True once per Monero lock transaction, the first time its confirmations reach the target. */
    static boolean announcesFinality(java.util.Set<String> announced, String txid, long confirmations) {
        return confirmations >= XMR_FINALITY_CONFIRMATIONS && announced.add(String.valueOf(txid));
    }

    @Override
    public void onBtcRedeemed(String swapId) {
        historyController.refreshHistoryList();
        // Step 13: BTC redeemed, begin XMR redemption
        swapsController.updateGui(() -> {
            swapsController.cancelButton.setDisable(false);
            swapsController.setProgress(13f / 15f);
            swapsController.setLockIcon(SwapIconType.BTC_REDEEM);
            Tooltip.install(swapsController.swapIcon, new Tooltip("BTC Redeemed"));
            swapsController.setSwapStatus("BTC redeemed - redeeming XMR...");
            swapsController.setSwapStatusDetail(swapId, "Swap ID");
            // Steps 1-3 have necessarily completed by the time BTC is redeemed;
            // a resumed swap never saw their individual callbacks, so set the
            // canonical labels instead of leaving the detail column blank.
            swapsController.setSwapStep(1, "sent");
            swapsController.setSwapStep(2, "confirmed");
            swapsController.setSwapStep(3, "confirmed");
            swapsController.setSwapStep(4, "claimed");
            swapsController.setSwapStep(5, "preparing");
        });

        printSwapLogLn("Alice has redeemed the Bitcoin! Redeeming Monero...");
    }

    @Override
    public void onStartRedeemXmrSync(String swapId) {
        // Step 14: Setup XMR Wallet
        // The redeem run owns the UI from here: the resume heartbeat would
        // otherwise keep printing "Still resuming swap..." alongside the XMR
        // sync heartbeat, which reads as two competing processes.
        swapsController.stopResumeHeartbeat();
        printSwapLogLn("Monero wallet setup, syncing... This could take a few minutes...");
        startXmrSyncHeartbeat();
        swapsController.updateGui(() -> {
            swapsController.setProgress(-1.0f);
            swapsController.setLockIcon(SwapIconType.SYNCING);
            swapsController.setSwapStatus("Preparing Monero redeem...");
            swapsController.setSwapStep(5, "preparing redeem");
            swapsController.cancelButton.setDisable(false);
        });
    }

    // Preparing the Monero redeem (daemon lookups, or a wallet-rpc sync on the
    // fallback path) is one blocking call with no progress events, so until the
    // sweep event arrives the log would otherwise sit silent and look hung.
    // Emit a periodic liveness line; cheap and only runs during that phase.
    private java.util.concurrent.ScheduledExecutorService xmrSyncHeartbeatExecutor;
    private volatile java.util.concurrent.ScheduledFuture<?> xmrSyncHeartbeatTask;

    /** How often the sweep phases say they are still working: about one Monero block time. */
    static final long SWEEP_HEARTBEAT_SECONDS = 120;
    static final long SYNC_HEARTBEAT_SECONDS = 30;
    /**
     * How often the wait for the Bitcoin lock to confirm says it is still waiting: long, because
     * Bitcoin blocks average 10 minutes and a slow one or a missed block is normal.
     */
    static final long BTC_LOCK_HEARTBEAT_SECONDS = 600;

    static String btcLockWaitLog(long seconds) {
        return "Still waiting for the Bitcoin lock transaction to be confirmed (" + (seconds / 60) + "m)...";
    }

    static String btcLockWaitStatus(long seconds) {
        return "BTC locked - waiting for confirmation (" + (seconds / 60) + "m)";
    }

    private void startXmrSyncHeartbeat() {
        startXmrHeartbeat(
                SYNC_HEARTBEAT_SECONDS,
                seconds -> "Still preparing Monero redeem (" + elapsed(seconds) + ")...",
                seconds -> "Still preparing the Monero redeem (" + elapsed(seconds) + ")... Looking up the lock and decoys over Tor can take a minute; a wallet-based redeem may also need to scan a long chain.");
    }

    /** Elapsed time at the n-th heartbeat tick: an exact multiple of the period. */
    static long heartbeatElapsed(long tick, long periodSeconds) {
        return swap.model.HeartbeatText.atTick(tick, periodSeconds);
    }

    /** Heartbeat time: "40s" under a minute, "2m" on whole minutes, otherwise "2m 30s". */
    static String elapsed(long seconds) {
        return swap.model.HeartbeatText.elapsed(seconds);
    }

    /** Status and log text for the wait for the Monero sweep to be confirmed. */
    static String sweepWaitStatus(long seconds) {
        return "Sweep broadcast - waiting for confirmation (" + elapsed(seconds) + ")";
    }

    static String sweepBroadcastNotice(String txid) {
        return "Monero sweep broadcast in transaction " + txid + ". Waiting for confirmation...";
    }

    static String sweepWaitLog(long seconds) {
        return "Still waiting for the Monero sweep to be confirmed (" + elapsed(seconds) + ")...";
    }

    static String sweepBuildLog(long seconds) {
        return "Still building and broadcasting the Monero sweep (" + elapsed(seconds) + ")...";
    }

    private void startXmrHeartbeat(long periodSeconds, java.util.function.LongFunction<String> status, java.util.function.LongFunction<String> log) {
        stopXmrSyncHeartbeat();
        if (xmrSyncHeartbeatExecutor == null) {
            xmrSyncHeartbeatExecutor = java.util.concurrent.Executors.newSingleThreadScheduledExecutor(runnable -> {
                Thread thread = new Thread(runnable, "xmr-sync-heartbeat");
                thread.setDaemon(true);
                return thread;
            });
        }
        // Count ticks rather than reading the clock, so the heartbeat reads exactly "2m", "4m"
        // (one Monero block apart) instead of drifting to "2m 1s".
        java.util.concurrent.atomic.AtomicLong ticks = new java.util.concurrent.atomic.AtomicLong();
        xmrSyncHeartbeatTask = xmrSyncHeartbeatExecutor.scheduleWithFixedDelay(() -> {
            long elapsedSeconds = heartbeatElapsed(ticks.incrementAndGet(), periodSeconds);
            swapsController.updateGui(() -> {
                if (xmrSyncHeartbeatTask != null) swapsController.setSwapStatus(status.apply(elapsedSeconds));
            });
            printSwapLogLn(log.apply(elapsedSeconds));
        }, periodSeconds, periodSeconds, java.util.concurrent.TimeUnit.SECONDS);
    }

    private void stopXmrSyncHeartbeat() {
        if (xmrSyncHeartbeatTask != null) {
            xmrSyncHeartbeatTask.cancel(false);
            xmrSyncHeartbeatTask = null;
        }
    }

    @Override
    public void onStartXmrSweep(String swapId) {
        // Step 14: Redeeming XMR
        swapsController.stopResumeHeartbeat();
        swapsController.updateGui(() -> {
            swapsController.cancelButton.setDisable(false);
            swapsController.setProgress(14f / 15f);
            swapsController.setLockIcon(SwapIconType.SWEEPING);
            swapsController.setSwapStatus("Sweeping to your wallet...");
            swapsController.setSwapStep(5, "sweeping");
        });

        printSwapLogLn("Sweeping to your wallet: building and broadcasting the sweep, then waiting for it to be confirmed.");
        // Replaces the sync heartbeat: building and broadcasting can take a while over Tor, and
        // the wait for a block after it is longer still, so keep saying what is happening.
        startXmrHeartbeat(
                SWEEP_HEARTBEAT_SECONDS,
                SwapListenerImpl::sweepBuildStatus,
                SwapListenerImpl::sweepBuildLog);
    }

    static String sweepBuildStatus(long seconds) {
        return "Building and broadcasting sweep (" + elapsed(seconds) + ")...";
    }

    @Override
    public void onXmrSweepBroadcast(String swapId, String sweepTxid) {
        // The daemon has the sweep; only a block is missing. Show the transaction so it can be
        // looked up, and keep a heartbeat going until the swap completes.
        swapsController.updateGui(() -> {
            swapsController.setSwapStatus(sweepWaitStatus(0));
            swapsController.setSwapStatusDetail(sweepTxid, "XMR sweep TX");
        });
        printSwapLogLn(sweepBroadcastNotice(sweepTxid));
        startXmrHeartbeat(
                SWEEP_HEARTBEAT_SECONDS,
                SwapListenerImpl::sweepWaitStatus,
                SwapListenerImpl::sweepWaitLog);
    }

    /** swapId -> {received, fee} piconero, as reported by the engine at the sweep (matrix N-15). */
    private final java.util.Map<String, long[]> xmrReceived = new java.util.concurrent.ConcurrentHashMap<>();

    @Override
    public void onXmrReceived(String swapId, long receivedPiconero, long feePiconero) {
        if (receivedPiconero < 0 || feePiconero < 0) return;
        xmrReceived.put(swapId, new long[]{receivedPiconero, feePiconero});
    }

    @Override
    public void onSwapCompleted(CompletedSwap completedSwap) {
        // Swap reached its terminal success state: stop its retry loop.
        GUISwap.appSwap.stopSwapClient(completedSwap.swapId());
        stopXmrSyncHeartbeat();
        swapsController.stopResumeHeartbeat();

        printSwapLogLn(LogType.SUCCESS, "Successfully transferred Monero to wallet for swap: " + completedSwap.swapId(), false, true);
        long[] received = xmrReceived.remove(completedSwap.swapId());
        if (received != null) printSwapLogLn(SwapReceived.logLine(received[0], received[1]));

        // Step 15: Redeemed XMR - 100% complete
        swapsController.updateGui(() -> {
            swapsController.reserveSwapProgressSpace();
            swapsController.cancelButton.setText("Clear");
            swapsController.setSwapStatus("Swap Success", "#27be4d");
            swapsController.markSwapTerminal();
            swapsController.completeSwapSteps();
            swapsController.setLockIcon(SwapIconType.CHECK);
            swapsController.cancelButton.setDisable(false);
            Tooltip.install(swapsController.swapIcon, new Tooltip("Swap Success"));
            swapsController.setSwapStatusDetail(completedSwap.swapId(), "Swap ID");
        });
        if (received != null) swapsController.showXmrReceived(received[0], received[1]);
    }

    @Override
    public void onSwapCancelPrepared(String swapId) {
        // The refund path is armed, but no transaction is on the network yet.
        // The swap is still locked and waiting for the timelock.
        swapsController.stopResumeHeartbeat();
        swapsController.updateGui(() -> {
            swapsController.cancelButton.setDisable(false);
            swapsController.setProgress(1.0f / 15.0f);
            swapsController.setSwapStatus(STATUS_REFUND_PENDING, "#be7327");
            swapsController.setSwapStatusDetail(swapId, "Waiting for the lock timelock; no cancel or refund transaction has been broadcast yet.");
            swapsController.setSwapStep(2, "confirmed");
            swapsController.setLockIcon(SwapIconType.REFUND_PENDING);
            Tooltip.install(swapsController.swapIcon, new Tooltip("Refund pending - waiting for timelock; no transaction broadcast"));
        });

        historyController.refreshHistoryList();
        printSwapLogLn(LogType.WARN, LOG_REFUND_PATH_ARMED, false, false);
        printSwapLogLn(LogType.INFO, LOG_REFUND_WAIT, false, false);
    }

    @Override
    public void onSwapCanceled(String swapId, String btcCancelTxid) {
        stopXmrSyncHeartbeat();
        // The cancel transaction was accepted for broadcast. Resume is over;
        // the client is now waiting for the cancel to confirm before it can
        // publish the refund. Stop the wallet-scan heartbeat.
        swapsController.stopResumeHeartbeat();
        swapsController.updateGui(() -> {
            swapsController.cancelButton.setDisable(false);
            swapsController.setProgress(-1.0f);
            swapsController.setSwapStatus(STATUS_CANCEL_BROADCAST, "#be7327");
            swapsController.setSwapStatusDetail(btcCancelTxid, "Cancel transaction broadcast; waiting for confirmation before the refund.");
            swapsController.markSwapCanceledStep();
            swapsController.setLockIcon(SwapIconType.CANCEL_PENDING);
            Tooltip.install(swapsController.swapIcon, new Tooltip("Cancel transaction broadcast - refund pending confirmation"));
        });

        historyController.refreshHistoryList();
        printSwapLogLn(LogType.WARN, "Bitcoin cancel transaction broadcast: " + btcCancelTxid, false, false);
        printSwapLogLn(LogType.INFO, "The cancel is now in the mempool. The refund transaction is created only after the cancel confirms; keep the client running - no action needed.", false, false);
    }

    @Override
    public void onSwapRefundBroadcast(String swapId, String btcRefundTxid) {
        stopXmrSyncHeartbeat();
        // The refund is on the network but not yet confirmed. This is a distinct
        // phase from the cancel broadcast and must not claim completion.
        swapsController.stopResumeHeartbeat();
        swapsController.updateGui(() -> {
            swapsController.cancelButton.setDisable(false);
            swapsController.setProgress(-1.0f);
            swapsController.setSwapStatus(STATUS_REFUND_BROADCAST, "#be7327");
            swapsController.setSwapStatusDetail(btcRefundTxid, "Refund transaction broadcast; waiting for confirmation.");
            swapsController.markSwapRefundBroadcastStep();
            swapsController.setLockIcon(SwapIconType.REFUND_PENDING);
            Tooltip.install(swapsController.swapIcon, new Tooltip("Refund broadcast - waiting for confirmation"));
        });

        historyController.refreshHistoryList();
        printSwapLogLn(LogType.WARN, "Bitcoin refund transaction broadcast: " + btcRefundTxid, false, false);
        printSwapLogLn(LogType.INFO, "The refund is in the mempool. Keep the client running until it confirms.", false, false);
    }

    @Override
    public void onSwapError(SwapError swapError) {
        swapsController.stopPreparingHeartbeat();
        swapsController.stopResumeHeartbeat();
        stopXmrSyncHeartbeat();
        historyController.refreshHistoryList();
        if (swapError.fatal()) { // currently always fatal, just here for if it's ever needed
            if (swapError.errorType().shouldRestart()) {
                // Stop the failing client before its listener-driven replacement starts,
                // so the old retry loop cannot spin alongside the new swap client.
                GUISwap.appSwap.stopSwapClient(swapError.swapId());
                restart(swapError.errorMessage(), swapError.swapId(), swapError.errorType());
            } else {
                // Fatal and not restartable: the retry loop must not re-invoke it.
                GUISwap.appSwap.stopSwapClient(swapError.swapId());
                if (swapError.errorType() == SwapErrorType.SwapDisconnected) {
                    // Disconnect
                    swapsController.updateGui(() -> {
                        swapsController.setProgress(0.0f);
                        swapsController.resetSwapDisplay();
                        swapsController.cancelButton.setDisable(false);
                    });

                    printSwapLogLn(swap.model.UserDisconnect.knownSafe() ? LogType.INFO : LogType.ERROR,
                            disconnectMessage(swap.model.UserDisconnect.knownSafe()), true, true);
                } else {
                    swapsController.updateGui(() -> {
                        // Error
                        swapsController.markSwapTerminal();
                        swapsController.reserveSwapProgressSpace();
                        swapsController.cancelButton.setText("Clear");
                        swapsController.cancelButton.setDisable(false);
                        swapsController.setSwapStatus(errorTitle(swapError), "#c0201a");
                        swapsController.setLockIcon(SwapIconType.CLOSE);
                        Tooltip.install(swapsController.swapIcon, new Tooltip(errorTitle(swapError)));
                        swapsController.setSwapStatusDetail(errorDetail(swapError), "Swap ID: " + swapError.swapId());
                        swapsController.failCurrentSwapStep();
                    });

                    printSwapLogLn(
                            LogType.ERROR,
                            "Swap " + swapError.swapId() + " failed [" + swapError.errorType() + "]: " + logError(swapError.errorMessage()),
                            false,
                            true);
                    long[] feeRefusal = sellerFeeRefusals.remove(swapError.swapId());
                    if (feeRefusal != null) offerSellerFeeRetry(feeRefusal);
                }
            }
        }
    }

    /** swapId -> {seller rate, our max, our estimate} sat/kvB, until its setup failure is shown. */
    private final java.util.Map<String, long[]> sellerFeeRefusals = new java.util.concurrent.ConcurrentHashMap<>();

    @Override
    public void onSellerFeeRateTooHigh(String swapId, long sellerRate, long ourMax, long ourEstimate) {
        // Arrives just before the setup failure itself; the dialog follows that failure's display.
        sellerFeeRefusals.put(swapId, new long[]{sellerRate, ourMax, ourEstimate});
        printSwapLogLn("Seller asked " + swap.model.SellerFeeAllowance.approxRate(sellerRate)
                + " for its swap transactions; Outpost accepts up to "
                + swap.model.SellerFeeAllowance.approxRate(ourMax) + " (network: "
                + swap.model.SellerFeeAllowance.approxRate(ourEstimate) + "). Nothing was locked.");
    }

    /** "Try another seller" (default) or accept this seller's rate for one new swap (S-01). */
    private void offerSellerFeeRetry(long[] refusal) {
        swapsController.updateGui(() -> {
            String peer = swapsController.libp2pPeerTextField.getText();
            javafx.stage.Stage owner = (javafx.stage.Stage) swapsController.cancelButton.getScene().getWindow();
            if (!swap.gui.controller.popups.PopupActiveSwapWarning.confirmSellerFee(owner, refusal[0], refusal[1], refusal[2])) {
                return;
            }
            swap.model.SellerFeeAllowance.allowOnce(peer, refusal[0]);
            printSwapLogLn("Accepted " + swap.model.SellerFeeAllowance.approxRate(refusal[0])
                    + " for the next swap with this seller only; retrying.");
            swapsController.onStartButtonClick();
        });
    }

    private static String errorTitle(SwapError swapError) {
        return swapError.errorType() == SwapErrorType.SwapPunished ? "Swap punished" : "Swap failed";
    }

    private static String errorDetail(SwapError swapError) {
        if (swapError.errorType() == SwapErrorType.SwapPunished) {
            return "Refund window missed - BTC was claimed";
        }
        return userFacingError(swapError.errorMessage());
    }

    /**
     * Shorten the native error for the fixed-width status box, but never replace
     * it with a tautology: the actual cause is what the user needs. The log
     * gets its own single-line form via {@link #logError(String)}.
     */
    static String userFacingError(String raw) {
        if (raw == null || raw.isBlank()) {
            return "Unexpected swap error";
        }
        String lowered = raw.toLowerCase(Locale.ROOT);
        if (lowered.contains("insufficient funds")
                || lowered.contains("failed to select transaction inputs")) {
            return "Not enough BTC for swap + fee.";
        }
        if (lowered.contains("verify alice") && lowered.contains("signature is invalid")) {
            return "Invalid seller signature.";
        }
        // Failures of the seller's side of the setup. The setup comes before any Bitcoin is locked, so
        // nothing was spent: say so, and say what to do (the sellers differ, another one usually works).
        if (lowered.contains("seller's offer refused")) {
            return "Seller's offer didn't match its quote. Nothing locked. Try another seller.";
        }
        if (lowered.contains("seller encountered a problem")) {
            return "Seller had a problem on its side. Nothing locked. Try another seller.";
        }
        if (lowered.contains("configured buy limit")) {
            return "Amount is outside this seller's limits. Try another seller.";
        }
        if (lowered.contains("xmr balance is currently too low")) {
            return "This seller has too little Monero. Try another seller.";
        }
        if (lowered.contains("does not accept incoming swap requests")) {
            return "This seller isn't accepting swaps. Try another seller.";
        }
        String oneLine = logError(raw);
        oneLine = oneLine.replaceFirst("(?i)^Protocol-5 setup failed:\\s*", "");
        oneLine = oneLine.replaceFirst("(?i)^Something went wrong during the swap setup protocol:\\s*", "");
        int sentenceEnd = oneLine.indexOf(". ");
        if (sentenceEnd > 0) {
            oneLine = oneLine.substring(0, sentenceEnd + 1);
        }
        // The square is a short status, not an error dump. Never append an
        // ellipsis here; txids are the only values allowed to use "...".
        if (oneLine.length() > 80) {
            return "Unexpected swap error.";
        }
        return oneLine;
    }

    /**
     * One clean line for the log: keep the actual failure, but do not dump an
     * anyhow/stack chain. Cause/backtrace sections are intentionally omitted.
     */
    static String logError(String raw) {
        if (raw == null || raw.isBlank()) {
            return "Unexpected swap error";
        }
        String oneLine = raw.replaceAll("\\s+", " ").trim();
        int cause = oneLine.indexOf("Caused by:");
        if (cause < 0) {
            cause = oneLine.indexOf("Stack backtrace:");
        }
        if (cause > 0) {
            oneLine = oneLine.substring(0, cause).trim();
        }
        return oneLine;
    }

    @Override
    public void onGenericSellerQuoteError(String swapId, String error) {
        // Transient, self-healing: restart() backs off and retries. Do not show
        // a terminal failure icon; that read as a permanent failure.
        if (!shouldRetry(false, swap.model.UserDisconnect.happened())) {
            restart(error, swapId, null); // the user cancelled: restart() ends it quietly
            return;
        }
        swapsController.updateGui(() -> {
            swapsController.setSwapStatus("Quote failed - reconnecting to seller...", "#be7327");
            swapsController.setLockIcon(SwapIconType.SYNCING);
            swapsController.cancelButton.setDisable(false);
            Tooltip.install(swapsController.swapIcon, new Tooltip("Retrying seller quote"));
            swapsController.setSwapStatusDetail(swapId, "Swap ID");
        });

        restart("Error getting quote from seller. Retrying..." + error, swapId, null);
    }

    @Override
    public void onSwapSafelyAborted(String swapId) {
        // Terminal state: stop the retry loop for this swap.
        GUISwap.appSwap.stopSwapClient(swapId);
        swapsController.stopResumeHeartbeat();

        swapsController.updateGui(() -> {
            swapsController.setSwapStatus("Swap aborted", "#c0201a");
            swapsController.setLockIcon(SwapIconType.CLOSE);
            swapsController.cancelButton.setDisable(false);
            Tooltip.install(swapsController.swapIcon, new Tooltip("Swap Aborted"));
            swapsController.setSwapStatusDetail(swapId, "Swap ID");
            swapsController.failCurrentSwapStep();
        });

        printSwapLogLn(LogType.ERROR, "Swap is in safely aborted state. Exiting.", false, true);
    }

    @Override
    public void onSwapRefunded(String swapId) {
        // Terminal state: stop the retry loop for this swap.
        GUISwap.appSwap.stopSwapClient(swapId);
        swapsController.stopResumeHeartbeat();

        historyController.refreshHistoryList();
        swapsController.updateGui(() -> {
            swapsController.markSwapTerminal();
            swapsController.reserveSwapProgressSpace();
            swapsController.cancelButton.setText("Clear");
            swapsController.setSwapStatus("Swap refunded", "#be7327");
            swapsController.setLockIcon(SwapIconType.CLOSE);
            swapsController.cancelButton.setDisable(false);
            Tooltip.install(swapsController.swapIcon, new Tooltip("Swap Refunded"));
            swapsController.setSwapStatusDetail(swapId, "Swap ID");
            swapsController.markSwapRefundedStep();
            swapsController.failCurrentSwapStep();
        });

        // Terminal refund: keep the seller/XMR/price/amount context visible so
        // the user can see which swap refunded. kill=true + clearFields=true
        // wiped the top context fields at the exact moment they mattered most.
        printSwapLogLn(LogType.WARN, "Swap " + swapId + " failed! You have been refunded (minus Bitcoin transaction fees).", false, true);
    }

    /** Log line for a swap whose seller kept the deposit after a partial refund. */
    public static String partialRefundLog(String swapId, long refundedSats, long depositSats) {
        return "Swap " + swapId + " failed and was partially refunded: about " + refundedSats
                + " sats came back (minus Bitcoin transaction fees); the seller kept its "
                + depositSats + " sat deposit.";
    }

    @Override
    public void onSwapPartiallyRefunded(String swapId, long refundedSats, long depositSats) {
        // Terminal state, like onSwapRefunded: stop the retry loop for this swap.
        GUISwap.appSwap.stopSwapClient(swapId);
        swapsController.stopResumeHeartbeat();

        historyController.refreshHistoryList();
        swapsController.updateGui(() -> {
            swapsController.markSwapTerminal();
            swapsController.reserveSwapProgressSpace();
            swapsController.cancelButton.setText("Clear");
            swapsController.setSwapStatus("Swap partially refunded", "#be7327");
            swapsController.setLockIcon(SwapIconType.CLOSE);
            swapsController.cancelButton.setDisable(false);
            Tooltip.install(swapsController.swapIcon, new Tooltip("Swap partially refunded"));
            swapsController.setSwapStatusDetail(swapId, "Swap ID");
            swapsController.markSwapRefundedStep();
            swapsController.failCurrentSwapStep();
        });

        printSwapLogLn(LogType.WARN, partialRefundLog(swapId, refundedSats, depositSats), false, true);
    }

    /**
     * Bitcoin locks already announced in full during this app run. Kept in memory only, so after
     * an app restart (a new run, with an empty log) the full lines print again.
     */
    private static final java.util.Set<String> announcedLocks = java.util.concurrent.ConcurrentHashMap.newKeySet();

    /** True the first time this lock is seen in this app run; later (automatic resumes) false. */
    static boolean announcesLockInFull(String btcLockTxid) {
        return announcedLocks.add(btcLockTxid == null ? "" : btcLockTxid);
    }

    /** The single line for a resume that follows an earlier announcement. */
    static String resumedLockLine(String btcLockTxid) {
        return "Resumed: your Bitcoin is locked in transaction " + btcLockTxid + ". The swap carries on from there.";
    }

    /** Test seam: a new app run has an empty memory. */
    static void forgetAnnouncedLocks() {
        announcedLocks.clear();
    }

    private static final java.util.concurrent.ConcurrentHashMap<String, Integer> restartCounts = new java.util.concurrent.ConcurrentHashMap<>();

    /**
     * Whether a failure may be retried automatically. A swap the user disconnected from before the
     * Bitcoin lock is never retried; after the lock a failure always resumes (audit H-2).
     */
    static boolean shouldRetry(boolean afterLock, boolean userDisconnected) {
        return afterLock || !userDisconnected;
    }

    /** What the log says after Disconnect: a pre-deposit cancel has nothing to resume. */
    static String disconnectMessage(boolean knownSafe) {
        if (knownSafe) {
            return "Disconnected. Nothing was locked, so there is nothing to resume.";
        }
        return "Disconnected from trade. Note that if there is Bitcoin locked in this trade you will need to resume soon, or risk being punished and having your Bitcoin taken from you.";
    }

    private void restart(String errorMessage, String swapId, SwapErrorType errorType) {
        boolean afterLock = errorType == SwapErrorType.SwapInterrupted;
        if (!shouldRetry(afterLock, swap.model.UserDisconnect.happened())) {
            // The user cancelled this swap: a seller error that arrives afterwards must not start it again.
            GUISwap.appSwap.stopSwapClient(swapId);
            printSwapLogLn(LogType.INFO, "Not retrying: you disconnected.", false, true);
            swapsController.updateGui(() -> {
                swapsController.setProgress(0.0f);
                swapsController.resetSwapDisplay();
                swapsController.cancelButton.setDisable(false);
            });
            return;
        }
        printSwapLogLn(LogType.ERROR, errorMessage, false, true);
        int count = restartCounts.merge(swapId, 1, Integer::sum);
        long delaySeconds = Math.min(60L * (1L << Math.min(count - 1, 4)), 600L);
        swapsController.updateGui(() -> {
            swapsController.cancelButton.setDisable(false);
            // Without this the panel kept saying "Preparing" while the swap was
            // actually failed and waiting out the backoff.
            swapsController.setSwapStatus("Retrying", "#d4a017");
            swapsController.setSwapStatusDetail(
                    restartDetail(afterLock, delaySeconds),
                    userFacingError(errorMessage));
        });
        if (count > 1) {
            printSwapLogLn(LogType.INFO, "Restart #" + count + " for this swap; backing off " + delaySeconds + "s before resuming.", false, true);
        }
        GUISwap.executorService.submit(() -> {
            try {
                Thread.sleep(delaySeconds * 1000L);
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
                return;
            }
            Platform.runLater(() -> {
                if (!shouldRetry(afterLock, swap.model.UserDisconnect.happened())) {
                    return; // disconnected while this retry was waiting
                }
                GUISwap.appSwap.restartXmrRpcProcesses();
                try {
                    Optional<SwapData> swapDataOptional = historyController.historyObservableList.stream().filter(swapData -> swapData.swapId().equals(swapId)).findAny();
                    if (afterLock) {
                        // Bitcoin may be locked: always resume this swap, never start a new one.
                        historyController.resumeSwap(swapId);
                        return;
                    }
                    swapDataOptional.ifPresentOrElse(swapData -> historyController.resumeSwap(swapData.swapId()), () -> swapsController.startSwap(swapsController.getMoneroAddressText(), swapsController.getLibp2pPeerText(), swapsController.getRefundAddress(), UUID.randomUUID().toString()));
                } catch (Exception ignored) {
                }
            });
        });
    }

    /** The status line while a restart waits out its backoff. */
    public static String restartDetail(boolean interrupted, long delaySeconds) {
        return interrupted
                ? "Swap interrupted; resuming in " + delaySeconds + "s. Keep the app open: Bitcoin may be locked."
                : "Setup failed; retrying in " + delaySeconds + "s";
    }
}
