package swap.process;

import org.bitcoinj.core.Coin;
import org.json.JSONObject;
import swap.gui.GUISwap;
import swap.gui.controller.MainController;
import swap.gui.controller.pages.LiquidityController;
import swap.model.AsbInitData;
import swap.model.AsbXmrBalanceData;
import swap.model.LogType;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;

public record ProcessAsb(Process process) {

    /**
     * The ASB log lives in the app's data folder, where the Liquidity page reads it. (It used to be a
     * hardcoded macOS path under {@code user.home}: wrong on Linux and Windows, and it ignored the
     * data folder the rest of the app was using.)
     */
    static Path asbLogPath(java.io.File dataDir) {
        return dataDir.toPath().resolve("logs").resolve("asb.log");
    }

    private static void appendAsbLog(String line) {
        try {
            Path path = asbLogPath(swap.lib.AppSwap.getSwapRootDir());
            Files.createDirectories(path.getParent());
            Files.writeString(path, line + System.lineSeparator(),
                    StandardOpenOption.CREATE, StandardOpenOption.APPEND);
        } catch (IOException ignored) {
            // Logging must never take down the ASB reader.
        }
    }

    // Init-failure backoff: consecutive init errors double the restart delay
    // (60 s floor, 10 min cap) so a dead endpoint cannot restart-loop the ASB
    // forever. ASB_INITIALIZED resets the counter. The pool forwarders make
    // the first retry succeed in the common case (failover already rotated).
    /** True the first time it is called for one ASB run (the flag), then false: one refresh per run. */
    static boolean refreshesSellersAfterRegistering(boolean[] alreadyRefreshed) {
        if (alreadyRefreshed[0]) {
            return false;
        }
        alreadyRefreshed[0] = true;
        return true;
    }

    /** The minute heartbeat while the ASB's Monero wallet syncs at start-up; null when none runs. */
    private static java.util.concurrent.ScheduledFuture<?> xmrSyncHeartbeat;

    /**
     * The heartbeat line after {@code minutes} of syncing. A new install's first sync can take
     * several minutes over Tor (measured 4.5 to 8, WORKLOG section 444) with nothing else in the log.
     */
    static String xmrSyncHeartbeatMessage(long minutes) {
        String elapsed = minutes == 1 ? "1 minute" : minutes + " minutes";
        return "[ASB] Still syncing the Monero wallet (" + elapsed + " so far). The first sync on a new install often takes 10 minutes or more over Tor.";
    }

    /** The Liquidity page's line while the ASB's Monero wallet syncs. */
    static String xmrSyncStatus(long minutes) {
        String head = minutes <= 0 ? "Syncing the ASB's Monero wallet..." : "Syncing the ASB's Monero wallet... " + (minutes == 1 ? "1 minute" : minutes + " minutes") + " so far.";
        return head + "\nThe first sync on a new install often takes 10 minutes or more over Tor. It happens once. Later starts take seconds.";
    }

    private static void setLiquidityStartupStatus(String text) {
        LiquidityController liquidity = LiquidityController.getInstance();
        if (liquidity != null) {
            liquidity.setStartupStatus(text);
        }
    }

    private static synchronized void startXmrSyncHeartbeat() {
        stopXmrSyncHeartbeat();
        // count the ticks: reading the clock rounded 299.9 s down to "4 minutes" twice in a row
        final java.util.concurrent.atomic.AtomicLong ticks = new java.util.concurrent.atomic.AtomicLong();
        xmrSyncHeartbeat = GUISwap.scheduledExecutorService.scheduleAtFixedRate(() -> {
            MainController controller = MainController.getInstance();
            long minutes = ticks.incrementAndGet();
            if (controller != null) {
                controller.printSwapLogLn(LogType.INFO, xmrSyncHeartbeatMessage(minutes), false);
            }
            setLiquidityStartupStatus(xmrSyncStatus(minutes));
        }, 60, 60, java.util.concurrent.TimeUnit.SECONDS);
    }

    private static synchronized void stopXmrSyncHeartbeat() {
        if (xmrSyncHeartbeat != null) {
            xmrSyncHeartbeat.cancel(false);
            xmrSyncHeartbeat = null;
        }
    }

    private static final java.util.concurrent.atomic.AtomicInteger INIT_FAILURES =
            new java.util.concurrent.atomic.AtomicInteger(0);

    /** Wall-clock time of the latest ASB_INITIALIZED (0 = never). */
    private static volatile long lastInitializedAtMs = 0L;

    /**
     * A restart queued by an init failure must not kill an ASB that has since
     * come up healthy. At launch two ASB starts can race the wallet RPC: the
     * loser fails to initialize and queues a restart, the winner initializes,
     * and the queued restart then killed the healthy ASB 60 s later.
     */
    static boolean shouldRestartAfterBackoff(long scheduledAtMs, long lastInitializedAtMs) {
        return lastInitializedAtMs <= scheduledAtMs;
    }

    /**
     * The swap-log line for a failed Monero wallet start. The ASB now sends the cause (most often the
     * Monero node not answering over Tor); the old text only guessed at a duplicate process.
     */
    static String xmrInitErrorMessage(JSONObject fields) {
        String cause = fields == null ? "" : fields.optString("error", "").trim();
        if (cause.isEmpty()) {
            return "[ASB] Error initializing Monero wallet. Check the Monero node in Settings, and that no other monero-wallet-rpc or asb process is running.";
        }
        if (cause.contains("no connection to daemon")) {
            return "[ASB] Error initializing Monero wallet: the Monero node did not answer over Tor (" + cause + "). Retrying. If it keeps failing, pick another node in Settings.";
        }
        return "[ASB] Error initializing Monero wallet: " + cause;
    }

    /** The swap-log line for a failed Bitcoin wallet start, with the cause the ASB sends. */
    static String btcInitErrorMessage(JSONObject fields) {
        String cause = fields == null ? "" : fields.optString("error", "").trim();
        if (cause.isEmpty()) {
            return "[ASB] Error initializing Bitcoin wallet. Check the Electrum server in Settings (Test connection).";
        }
        return "[ASB] Error initializing Bitcoin wallet: " + cause + ".";
    }

    public static void restartWithBackoff(String walletKind) {
        final long scheduledAtMs = System.currentTimeMillis();
        int failures = INIT_FAILURES.incrementAndGet();
        long delaySec = Math.min(60L << Math.min(failures - 1, 4), 600L);
        MainController mainController = MainController.getInstance();
        if (mainController != null) {
            mainController.printSwapLogLn(LogType.WARN,
                    "[ASB] " + walletKind + " wallet init failed (attempt " + failures
                            + ") — restarting in " + delaySec + "s.", false);
        }
        GUISwap.scheduledExecutorService.schedule(() -> {
            if (!shouldRestartAfterBackoff(scheduledAtMs, lastInitializedAtMs)) {
                System.out.println("[ASB] Skipping queued restart: the ASB initialized after the failure.");
                return;
            }
            try {
                GUISwap.appAsb.restart();
            } catch (Throwable failure) {
                // A swallowed exception here left the ASB dead with no log and
                // no further retry (observed 2026-11-06). Log and re-arm.
                MainController controller = MainController.getInstance();
                if (controller != null) {
                    controller.printSwapLogLn(LogType.ERROR,
                            "[ASB] Restart failed: " + failure.getMessage()
                                    + " — retrying with backoff.", false);
                }
                restartWithBackoff(walletKind);
            }
        }, delaySec, java.util.concurrent.TimeUnit.SECONDS);
    }

    public ProcessAsb {
        GUISwap.executorService.submit(() -> {
            BufferedReader reader = new BufferedReader(new InputStreamReader(process.getErrorStream()));
            boolean[] rawErrorShown = {false};
            // the seller list is refreshed once more when this ASB first registers with a rendezvous point
            boolean[] refreshedAfterRegistering = {false};
            // A new ASB process starts with a clean live-swap slate; resumed
            // swaps re-enter through LIQUIDITY_RERESERVED / Advancing state.
            LiquidityController liquidityUi = LiquidityController.getInstance();
            if (liquidityUi != null) liquidityUi.clearAsbSwaps();
            String line;
            try {
                while (((line = reader.readLine()) != null) && process.isAlive()) {
                    appendAsbLog(line);
                    MainController mainController = MainController.getInstance();
                    JSONObject json;
                    try {
                        json = new JSONObject(line);
                    } catch (Exception nonJson) {
                        // Keep raw stderr in the console, but surface only one
                        // concise error line to the user; cause chains and panic
                        // backtraces are noise in the swap log.
                        System.err.println("[ASB] " + line);
                        if (!rawErrorShown[0] && line.startsWith("Error: ") && mainController != null) {
                            rawErrorShown[0] = true;
                            mainController.printSwapLogLn(LogType.ERROR, "[ASB] " + line, false);
                        }
                        continue;
                    }
                    try {
                    JSONObject fields = json.getJSONObject("fields");
                    if (fields.has("message")) {
                        String message = fields.getString("message");
                        // ASB liquidity markers carry a human-readable suffix
                        // (e.g. "ASB_LIQUIDITY_RESERVED for incoming swap setup");
                        // match the event code so those cases actually fire.
                        String messageKey = message.startsWith("ASB_") && message.contains(" ")
                                ? message.substring(0, message.indexOf(' '))
                                : message;
                        System.err.println(line);
                        // PROXY LOGGING
                        if (message.equals("Not using SOCKS5 proxy") || message.equals("Using SOCKS5 proxy at")) {
                            if (fields.has("proxy_string")) {
                                String proxyAddress = fields.getString("proxy_string");
                                mainController.printSwapLogLn(LogType.INFO, "[ASB] " + message + " " + proxyAddress, false);
                            } else {
                                mainController.printSwapLogLn(LogType.INFO, "[ASB] " + message, false);
                            }
                        }
                        // PROXY LOGGING

                        // ASB INIT DATA
                        switch (messageKey) {
                            case "ASB_INITIALIZED" -> {
                                INIT_FAILURES.set(0);
                                lastInitializedAtMs = System.currentTimeMillis();
                                String moneroAddress = fields.getString("monero_address");
                                String peerId = fields.getString("asb_peer_id");
                                String multiaddr = fields.getString("multiaddr");
                                long total = Long.parseLong(fields.getString("monero_balance.balance"));
                                long unlocked = Long.parseLong(fields.getString("monero_balance.unlocked_balance"));
                                long locked = total - unlocked;
                                Coin bitcoinBalance = Coin.valueOf(Long.parseLong(fields.getString("bitcoin_balance")));
                                AsbInitData asbInitData = new AsbInitData(peerId, multiaddr, total, unlocked, locked, bitcoinBalance, moneroAddress);
                                MainController.asbListener.onAsbInitialized(asbInitData);
                            }
                            case "ASB_INITIALIZED_MONERO_WALLET" ->
                                    mainController.printSwapLogLn(LogType.INFO, "[ASB] Opened Monero wallet", false);
                            case "ASB_SYNCING_MONERO_WALLET" -> {
                                mainController.printSwapLogLn(LogType.INFO, "[ASB] Syncing Monero wallet...", false);
                                setLiquidityStartupStatus(xmrSyncStatus(0));
                                startXmrSyncHeartbeat();
                            }
                            case "ASB_SYNCED_MONERO_WALLET" -> {
                                stopXmrSyncHeartbeat();
                                String duration = fields.getString("duration");
                                mainController.printSwapLogLn(LogType.INFO, "[ASB] Synced Monero wallet in " + duration + " seconds", false);
                            }
                            case "ASB_INITIALIZED_BITCOIN_WALLET" ->
                                    mainController.printSwapLogLn(LogType.INFO, "[ASB] Opened Bitcoin wallet", false);
                            case "ASB_SYNCING_BITCOIN_WALLET" -> {
                                    mainController.printSwapLogLn(LogType.INFO, "[ASB] Syncing Bitcoin wallet...", false);
                                    setLiquidityStartupStatus("Syncing the ASB's Bitcoin wallet through the Electrum server...");
                                }
                            case "ASB_SYNCED_BITCOIN_WALLET" -> {
                                String duration = fields.getString("duration");
                                mainController.printSwapLogLn(LogType.INFO, "[ASB] Synced Bitcoin wallet in " + duration + " seconds", false);
                            }
                            case "ASB_SETTING_UP_LIBP2P_SWARM" ->
                                    mainController.printSwapLogLn(LogType.INFO, "[ASB] Setting up libp2p swarm...", false);
                            case "Registered with rendezvous node" -> {
                                // The refresh at ASB start runs before registration (that takes a while
                                // over Tor), so it never found this ASB; refresh once more now, shortly
                                // after, so the rendezvous point has it listed.
                                if (refreshesSellersAfterRegistering(refreshedAfterRegistering)) {
                                    GUISwap.scheduledExecutorService.schedule(() -> javafx.application.Platform.runLater(() -> {
                                        MainController controller = MainController.getInstance();
                                        if (controller != null && controller.getSwapsController() != null) {
                                            controller.getSwapsController().onRefreshSellersButtonClick();
                                        }
                                    }), 5, java.util.concurrent.TimeUnit.SECONDS);
                                }
                            }
                            case "ASB_REGISTERING_ADDRESS_WITH_RENDEZVOUS" -> {
                                String externalAddress = fields.getString("external_address");
                                mainController.printSwapLogLn(LogType.INFO, "[ASB] Registering external address with rendezvous: " + externalAddress, false);
                            }
                            case "ASB_XMR_BALANCE_DATA" -> {
                                if (!fields.has("error")) {
                                    long total = Long.parseLong(fields.getString("balance.balance"));
                                    long unlocked = Long.parseLong(fields.getString("balance.unlocked_balance"));
                                    MainController.asbListener.onAsbXmrBalanceData(new AsbXmrBalanceData(total, unlocked, ""));
                                } else {
                                    MainController.asbListener.onAsbXmrBalanceData(new AsbXmrBalanceData(0, 0, fields.getString("error")));
                                }
                            }
                            case "ASB_ERROR_INITIALIZING_XMR_WALLET" -> {
                                stopXmrSyncHeartbeat();
                                mainController.printSwapLogLn(LogType.ERROR, xmrInitErrorMessage(fields), false);
                                restartWithBackoff("XMR");
                            }
                            case "ASB_ERROR_INITIALIZING_BTC_WALLET" -> {
                                mainController.printSwapLogLn(LogType.ERROR, btcInitErrorMessage(fields), false);
                                restartWithBackoff("BTC");
                            }
                            case "ASB_EARNED_BTC" -> {
                                if (fields.has("earned")) {
                                    long earnedSatoshis = fields.getLong("earned");
                                    Coin earnedCoins = Coin.valueOf(earnedSatoshis);
                                    mainController.printSwapLogLn(LogType.SUCCESS, "[ASB] Earned " + earnedCoins.toFriendlyString() + "!", false);
                                    LiquidityController liquidityController = LiquidityController.getInstance();
                                    if (liquidityController != null) liquidityController.onBtcEarned(fields.optString("swap_id", ""), earnedSatoshis, fields.optLong("xmr_sold_piconero", 0));
                                }
                            }
                            case "ASB_SWAP_COMPLETE" -> {
                                String swapId = fields.getString("swap_id");
                                String state = fields.getString("state").toUpperCase();
                                String log = "[ASB] Swap " + swapId + " completed with state: " + state;
                                if (state.equals("BTC IS REDEEMED"))
                                    mainController.printSwapLogLn(LogType.SUCCESS, log, false);
                                else
                                    mainController.printSwapLogLn(LogType.HIGHLIGHT, log, false);
                                trackAsbSwapTerminal(json, fields);
                            }
                            case "ASB_SWAP_FAIL" -> {
                                String swapId = fields.getString("swap_id");
                                mainController.printSwapLogLn(LogType.ERROR, "[ASB] Swap " + swapId + " failed with error: " + fields.getString("error"), false);
                                trackAsbSwapTerminal(json, fields);
                            }
                            case "Electrum verification failed, retrying" -> {
                                mainController.printSwapLogLn(LogType.WARN,
                                        "[ASB] Input verification backend failed: "
                                                + fields.optString("error", "unknown Electrum error"),
                                        false);
                            }
                            case "ASB_SWAP_RESUMING" -> {
                                mainController.printSwapLogLn(LogType.HIGHLIGHT, "[ASB] Resuming unfinished swap " + fields.getString("swap_id")
                                        + " from state: " + fields.optString("state", "unknown"), false);
                                noteResumedState(fields.getString("swap_id"), fields.optString("state", ""));
                            }
                            case "ASB_V5_RESUME_FAILED:" -> {
                                mainController.printSwapLogLn(LogType.ERROR, "[ASB] Could not resume swap: " + message.substring(message.indexOf(' ') + 1), false);
                            }
                            case "ASB_V5_RESUME_SKIP:" -> {
                                mainController.printSwapLogLn(LogType.WARN, "[ASB] Skipped resuming a swap: " + message.substring(message.indexOf(' ') + 1), false);
                            }
                            case "ASB_SWAP_STARTED" -> {
                                mainController.printSwapLogLn(LogType.HIGHLIGHT, "[ASB] Swap " + fields.getString("swap_id") + " started.", false);
                                trackAsbSwapState(json, fields, startedLabel(fields.getString("swap_id")));
                            }
                            case "ASB_BTC_LOCK_SEEN" -> {
                                mainController.printSwapLogLn(LogType.INFO, "[ASB] Swap " + fields.getString("swap_id") + " BTC lock transcation seen. Waiting on confirmation...", false);
                                trackAsbSwapState(json, fields, AsbStepLabels.BTC_SEEN);
                            }
                            case "ASB_BTC_LOCK_SEEN_ERR" -> {
                                mainController.printSwapLogLn(LogType.WARN, "[ASB] Swap " + fields.getString("swap_id") + " BTC lock transcation was not seen in mempool in time.", false);
                                trackAsbSwapState(json, fields, AsbStepLabels.BTC_NOT_SEEN);
                            }
                            case "ASB_BTC_LOCK" -> {
                                mainController.printSwapLogLn(LogType.INFO, "[ASB] Swap " + fields.getString("swap_id") + " BTC lock transcation confirmed.", false);
                                trackAsbSwapState(json, fields, AsbStepLabels.BTC_CONFIRMED);
                            }
                            case "ASB_BTC_LOCK_ERR" -> {
                                mainController.printSwapLogLn(LogType.WARN, "[ASB] Swap " + fields.getString("swap_id") + " BTC lock transcation did not get enough confirmations in time.", false);
                                trackAsbSwapState(json, fields, AsbStepLabels.BTC_NOT_CONFIRMED);
                            }
                            case "ASB_XMR_LOCK_PROOF" -> {
                                mainController.printSwapLogLn(LogType.INFO, "[ASB] Swap " + fields.getString("swap_id") + " XMR locked: " + fields.getString("txid"), false);
                                trackAsbSwapState(json, fields, AsbStepLabels.XMR_WAIT_BUYER);
                            }
                            case "Successfully initiated Monero transfer" -> {
                                String swapId = swapIdFor(json, fields);
                                mainController.printSwapLogLn(
                                        LogType.INFO,
                                        "[ASB] Swap " + swapId + " broadcast XMR lock: "
                                                + fields.optString("tx_id", "?")
                                                + " (" + fields.optString("amount", "?") + ")",
                                        false);
                                trackAsbSwapState(json, fields, AsbStepLabels.XMR_LOCKING);
                            }
                            case "Waiting for Monero transaction finality" -> {
                                String swapId = swapIdFor(json, fields);
                                mainController.printSwapLogLn(
                                        LogType.INFO,
                                        "[ASB] Swap " + swapId + " waiting for XMR lock finality (target "
                                                + fields.optString("target_confirmations", "?") + ")",
                                        false);
                                trackAsbSwapState(json, fields, AsbStepLabels.XMR_CONFIRMING);
                            }
                            case "Dojo broadcast timed out; falling back to Electrum" -> {
                                String swapId = swapIdFor(json, fields);
                                String kind = fields.optString("kind", "transaction");
                                mainController.printSwapLogLn(
                                        LogType.WARN,
                                        "[ASB] Swap " + swapId + " " + kind
                                                + " broadcast: Dojo timed out; using Electrum ("
                                                + fields.optString("txid", "?") + ")",
                                        false);
                            }
                            case "Constructed Bitcoin spending transaction" -> {
                                // Low-level debug event; it may be emitted during
                                // reconstruction/verification after the actual
                                // broadcast and is not a swap milestone. Keep it in
                                // the raw ASB log, but do not surface it here.
                            }
                            case "Advancing state" -> {
                                // State-machine transitions that have no dedicated
                                // marker: surface the interesting middle/late
                                // states; started/lock-seen/locked and the terminal
                                // states already have their own cases above/below.
                                // The swap id lives in the tracing span, not in fields.
                                String swapId = swapIdFor(json, fields);
                                String state = fields.getString("state");
                                if (isTerminalAsbState(state)) {
                                    trackAsbSwapTerminal(json, fields);
                                } else {
                                    trackAsbSwapState(json, fields, asbStateLabel(state));
                                }
                                switch (state) {
                                    case "started", "bitcoin lock transaction in mempool", "btc is locked",
                                         "xmr lock transfer proof sent", "btc is redeemed" -> {
                                        // covered by dedicated markers
                                    }
                                    case "xmr lock transaction sent" ->
                                            mainController.printSwapLogLn(LogType.INFO, "[ASB] Swap " + swapId + " locked XMR; waiting for confirmations...", false);
                                    case "xmr is locked" ->
                                            mainController.printSwapLogLn(LogType.INFO, "[ASB] Swap " + swapId + " XMR lock confirmed.", false);
                                    case "encrypted signature is learned" ->
                                            mainController.printSwapLogLn(LogType.INFO, "[ASB] Swap " + swapId + " received Bob's encrypted signature.", false);
                                    case "bitcoin redeem transaction published" ->
                                            mainController.printSwapLogLn(LogType.INFO, "[ASB] Swap " + swapId + " redeem published; waiting for confirmations...", false);
                                    default ->
                                            mainController.printSwapLogLn(LogType.INFO, "[ASB] Swap " + swapId + " state: " + state, false);
                                }
                            }
                            case "Published Bitcoin transaction" -> {
                                String kind = fields.has("kind") ? fields.getString("kind") : "unknown";
                                mainController.printSwapLogLn(LogType.INFO, "[ASB] Published Bitcoin " + kind + " transaction: " + fields.getString("txid"), false);
                            }
                            case "Received encrypted signature" -> {
                                // covered by the "encrypted signature is learned" state advance
                            }
                            case "ASB_XMR_LOCK_RETRY" -> {
                                mainController.printSwapLogLn(LogType.WARN, "[ASB] Swap " + fields.getString("swap_id") + " Monero lock transfer failed, retrying in " + fields.getString("retry_in") + "s: " + fields.getString("error"), false);
                                trackAsbSwapState(json, fields, AsbStepLabels.XMR_RETRYING);
                            }
                            case "ASB_XMR_LOCK_RECONCILED" -> {
                                mainController.printSwapLogLn(LogType.WARN, "[ASB] Swap " + fields.getString("swap_id") + " found an already-broadcast XMR lock transaction (" + fields.getString("tx") + "), adopting it.", false);
                                trackAsbSwapState(json, fields, AsbStepLabels.XMR_FOUND);
                            }
                            case "ASB_LIQUIDITY_RESERVED" -> {
                                mainController.printSwapLogLn(LogType.INFO, "[ASB] Reserved " + fields.getString("xmr") + " for incoming swap (" + fields.getString("buy") + "); " + fields.getString("remaining") + " still available.", false);
                                trackAsbSwapState(json, fields, AsbStepLabels.RESERVED);
                                LiquidityController liquidityController = LiquidityController.getInstance();
                                if (liquidityController != null) liquidityController.onLiquidityReserved(fields.getString("xmr"));
                            }
                            case "ASB_LIQUIDITY_KEYED" -> {
                                // internal accounting handoff - kept in the ASB raw log only
                            }
                            case "ASB_LIQUIDITY_RERESERVED" -> {
                                // startup re-arm of a resumed swap's reservation - mirror it
                                trackAsbSwapState(json, fields, AsbStepLabels.RESERVED);
                                LiquidityController liquidityController = LiquidityController.getInstance();
                                if (liquidityController != null && fields.has("amount")) liquidityController.onLiquidityReserved(fields.getString("amount"));
                            }
                            case "ASB_LIQUIDITY_RELEASED" -> {
                                // terminal-state release - mirror it so the donut stays truthful
                                trackAsbSwapTerminal(json, fields);
                                LiquidityController liquidityController = LiquidityController.getInstance();
                                if (liquidityController != null && fields.has("amount")) liquidityController.onLiquidityReleased(fields.getString("amount"));
                            }
                            case "ASB_TRANSFER_PROOF_ACK" -> {
                                // Bob acknowledged the proof - the swap's happy path
                                // continues silently; surfaced only in the raw log.
                            }
                            case "ASB_REDEEM_PUBLISHED" -> {
                                mainController.printSwapLogLn(LogType.INFO, "[ASB] Swap " + fields.getString("swap_id")
                                        + " BTC redeem tx broadcast: " + fields.getString("redeem_txid")
                                        + "; waiting for confirmations", false);
                                // The swap is still live (waiting to be paid): keep it in
                                // the active strip with the right label.
                                trackAsbSwapState(json, fields, AsbStepLabels.CLAIMING);
                            }
                            case "ASB_BTC_REDEEM_ERR" -> {
                                mainController.printSwapLogLn(LogType.WARN, "[ASB] Swap " + fields.getString("swap_id") + " failed to redeem BTC: " + fields.getString("error"), false);
                                trackAsbSwapState(json, fields, AsbStepLabels.CLAIM_FAILED);
                            }
                            // TODO: Revisit these cases. Don't seem to ever being hit.
                            case "ASB_CANCEL_TX" -> {
                                mainController.printSwapLogLn(LogType.WARN, "[ASB] Swap " + fields.getString("swap_id") + " cancel transaction broadcasted: " + fields.getString("txid"), false);
                                trackAsbSwapState(json, fields, AsbStepLabels.CANCELLING);
                            }
                            case "ASB_REFUND" -> {
                                mainController.printSwapLogLn(LogType.HIGHLIGHT, "[ASB] Swap " + fields.getString("swap_id") + " XMR successfully refunded.", false);
                                trackAsbSwapTerminal(json, fields);
                            }
                            case "ASB_PUNISH_TX" -> {
                                mainController.printSwapLogLn(LogType.HIGHLIGHT, "[ASB] Swap " + fields.getString("swap_id") + " punish transaction broadcasted: " + fields.getString("txid"), false);
                                trackAsbSwapState(json, fields, AsbStepLabels.PUNISHING);
                            }
                            case "ASB_REDEEM_TX" -> {
                                mainController.printSwapLogLn(LogType.HIGHLIGHT, "[ASB] Swap " + fields.getString("swap_id") + " redeem transaction successfully broadcasted: " + fields.getString("txid"), false);
                                trackAsbSwapState(json, fields, AsbStepLabels.CLAIMING);
                            }
                        }
                        // ASB INIT DATA
                    }
                    } catch (Exception dispatchError) {
                        System.err.println("ASB log dispatch error (line skipped): " + dispatchError);
                    }
                }

                // The log stream ends when the ASB process dies. A graceful shutdown
                // clears the running flag first, so a still-true flag here means the
                // ASB exited on its own and must be surfaced to the user.
                stopXmrSyncHeartbeat(); // the ASB's log ended: nothing is syncing any more
                if (!process.isAlive() && GUISwap.appAsb != null && GUISwap.appAsb.getRunningAsb()) {
                    GUISwap.appAsb.setRunningAsb(false);
                    MainController mainController = MainController.getInstance();
                    if (mainController != null) {
                        mainController.printSwapLogLn(LogType.ERROR,
                                "[ASB] ASB process exited unexpectedly (exit code " + process.exitValue() + ").", false);
                    }
                }
                if (liquidityUi != null) liquidityUi.clearAsbSwaps();
            } catch (IOException e) {
                if (liquidityUi != null) liquidityUi.clearAsbSwaps();
                throw new RuntimeException(e);
            }
        });
    }

    private static void trackAsbSwapState(JSONObject json, JSONObject fields, String state) {
        LiquidityController liquidityController = LiquidityController.getInstance();
        if (liquidityController != null) {
            liquidityController.onAsbSwapState(swapIdFor(json, fields), state);
        }
        refreshAsbHistory();
    }

    private static void trackAsbSwapTerminal(JSONObject json, JSONObject fields) {
        LiquidityController liquidityController = LiquidityController.getInstance();
        if (liquidityController != null) {
            liquidityController.onAsbSwapTerminal(swapIdFor(json, fields));
        }
        refreshAsbHistory();
    }

    /**
     * The activity strip (TOTAL / COMPLETED / ACTIVE / SUCCESS) is computed from
     * the persisted ASB history, which was only reloaded when the taker side
     * refreshed. Reload it when the ASB reports progress so the strip follows
     * the swap instead of showing the state from the last unrelated refresh.
     */
    private static void refreshAsbHistory() {
        MainController main = MainController.getInstance();
        if (main != null && main.getHistoryController() != null) {
            main.getHistoryController().refreshAsbHistoryList();
        }
    }

    // A resumed swap logs ASB_SWAP_RESUMING (its saved state) and then ASB_SWAP_STARTED, which would
    // otherwise show it as "Setup" on the Liquidity page (2026-11-14: a swap at XMR locked read Setup).
    private static final java.util.Map<String, String> RESUMED_STATES = new java.util.concurrent.ConcurrentHashMap<>();

    static void noteResumedState(String swapId, String state) {
        if (swapId != null && state != null && !state.isBlank()) {
            RESUMED_STATES.put(swapId, asbStateLabel(state));
        }
    }

    /** "Setup" for a new swap, or the saved state's label for one that was just resumed. */
    static String startedLabel(String swapId) {
        String resumed = RESUMED_STATES.remove(swapId);
        return resumed != null ? resumed : AsbStepLabels.ACCEPTED;
    }

    private static String asbStateLabel(String state) {
        return AsbStepLabels.forState(state);
    }

    private static boolean isTerminalAsbState(String state) {
        if (state == null) return false;
        return switch (state) {
            case "btc is redeemed", "btc is cancelled", "btc is refunded", "xmr is refunded",
                 "btc is early refunded", "btc is punished", "safely aborted" -> true;
            default -> false;
        };
    }

    private static String swapIdFor(JSONObject json, JSONObject fields) {
        if (fields.has("swap_id")) {
            return fields.getString("swap_id");
        }
        JSONObject span = json.optJSONObject("span");
        if (span != null && span.has("id")) {
            return span.getString("id");
        }
        return "unknown";
    }

    public static boolean shutdown(String pid) {
        System.out.println("Shutting down ASB with PID: " + pid);
        return ProcessGraceful.shutdownPid(pid, "asb");
    }

    public boolean shutdown() {
        if (process != null) {
            System.out.println("Shutting down ASB: " + process.pid());
            return ProcessGraceful.shutdown(process);
        }

        return false;
    }
}
