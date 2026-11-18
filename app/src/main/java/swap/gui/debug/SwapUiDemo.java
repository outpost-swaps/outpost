package swap.gui.debug;

import javafx.application.Platform;
import javafx.scene.Scene;
import javafx.scene.image.Image;
import javafx.scene.input.KeyCode;
import javafx.scene.input.KeyEvent;
import javafx.scene.layout.GridPane;
import javafx.stage.Stage;
import swap.gui.controller.pages.SwapsController;
import swap.gui.image.AddressQrCode;
import swap.lib.AppSwap;
import swap.model.SwapIconType;

import java.io.File;
import java.util.ArrayList;
import java.util.List;

/**
 * Dev-only harness: walks the swaps page through every in-progress display
 * state with fake data, using the same SwapsController API a real swap
 * drives, so the visuals can be reviewed without running a live swap.
 *
 * Developer-only: enabled solely by OUTPOST_SWAPS_UI_DEMO=1 in the environment (a release launcher
 * never sets it). Step with Alt+RIGHT / Alt+LEFT.
 */
public final class SwapUiDemo {
    private static final String TESTNET_BTC_ADDRESS = "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx";
    private static final String TESTNET_XMR_ADDRESS = "87BqQYkugEzh6Tuyotm2uc3DzJzKM6MuZaC161e6u1TsQxxPmXVPHpdNRyK47JY4d1hhbe25YVz4e9vTXCLDxvHkRXEAeBC";
    private static final String SELLER_MULTIADDR = "/dns4/hermes.eigenwallet.org/tcp/443/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
    private static final String BTC_LOCK_TXID = "6a1f8d0e3f6e2c4b7a9d1e5f8b2c6a4d0e3f7b1c5a9d2e6f8b4c0a3d7e1f5b9c";
    private static final String XMR_LOCK_TXID = "9c8b7a6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b";
    private static final String CANCEL_TXID = "1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c";
    private static final String REFUND_TXID = "79218d818d5f974f80c0f22e4c9c4437459b84460eeb99eee0065ea0a6366f99";
    private static final String SWAP_ID = "40ab3c9d-2f39-4b0c-9a6e-51c8d2f7e9a4";

    private static final List<Runnable> STEPS = new ArrayList<>();
    private static SwapsController swapsController;
    private static GridPane activePane;
    private static Scene handlerScene;
    private static int stepIndex = 0;

    private SwapUiDemo() {
    }

    public static void initIfEnabled(SwapsController controller, GridPane swapsPane) {
        if (!isEnabled()) return;
        swapsController = controller;
        activePane = swapsPane;
        buildSteps();
        stepIndex = 0;
        String configuredStep = System.getenv("OUTPOST_SWAPS_UI_DEMO_STEP");
        if (configuredStep != null) {
            try {
                stepIndex = Math.max(0, Math.min(Integer.parseInt(configuredStep), STEPS.size() - 1));
                Platform.runLater(SwapUiDemo::applyStep);
            } catch (NumberFormatException ignored) {
            }
        }
        swapsPane.sceneProperty().addListener((observable, oldScene, newScene) -> {
            if (newScene != null) installKeyHandler(newScene);
        });
        if (swapsPane.getScene() != null) installKeyHandler(swapsPane.getScene());
        System.out.println("[SwapUiDemo] enabled: Alt+RIGHT = next state, Alt+LEFT = previous state (" + STEPS.size() + " states)");
    }

    /** Developer-only: the environment variable alone. A file in the data folder no longer enables it. */
    static boolean isEnabled() {
        return isEnabledBy(System.getenv("OUTPOST_SWAPS_UI_DEMO"));
    }

    static boolean isEnabledBy(String envValue) {
        return "1".equals(envValue);
    }

    private static void installKeyHandler(Scene scene) {
        if (scene == handlerScene) return;
        handlerScene = scene;
        scene.addEventFilter(KeyEvent.KEY_PRESSED, event -> {
            GridPane pane = activePane;
            if (pane == null || pane.getScene() != scene || !pane.isVisible()) return;
            if (!event.isAltDown()) return;
            if (event.getCode() == KeyCode.RIGHT) {
                stepIndex = Math.min(stepIndex + 1, STEPS.size() - 1);
                applyStep();
                event.consume();
            } else if (event.getCode() == KeyCode.LEFT) {
                stepIndex = Math.max(stepIndex - 1, 0);
                applyStep();
                event.consume();
            }
        });
    }

    private static void applyStep() {
        if (swapsController == null) return;
        boolean swapActive = stepIndex > 0 && stepIndex < STEPS.size() - 1;
        swapsController.cancelButton.setDisable(!swapActive);
        STEPS.get(stepIndex).run();
        Platform.runLater(() -> {
            GridPane pane = swapsController.swapsPane;
            System.out.println("[SwapUiDemo] state " + (stepIndex + 1) + "/" + STEPS.size()
                    + " paneH=" + Math.round(pane.getHeight())
                    + " paneMinH=" + Math.round(pane.minHeight(pane.getWidth())));
            if (pane.getScene() != null && pane.getScene().getWindow() instanceof Stage stage) {
                stage.setTitle("Outpost — UI demo " + (stepIndex + 1) + "/" + STEPS.size());
            }
        });
    }

    private static void scheduleSellerRefresh() {
        Thread thread = new Thread(() -> {
            for (int i = 0; i < 240; i++) {
                try {
                    Thread.sleep(1000);
                } catch (InterruptedException e) {
                    return;
                }
                if (!swapsController.sellersObservableList.isEmpty()) {
                    Platform.runLater(() -> {
                        if (SELLER_MULTIADDR.equals(swapsController.libp2pPeerTextField.getText())
                                && swapsController.swapContextBox.isVisible()) {
                            swapsController.libp2pPeerTextField.setText(swapsController.sellersObservableList.get(0).multiaddr());
                            swapsController.refreshSwapContext();
                        }
                    });
                    return;
                }
            }
        }, "swap-ui-demo-seller-refresh");
        thread.setDaemon(true);
        thread.start();
    }

    private static void buildSteps() {
        if (!STEPS.isEmpty()) return;
        STEPS.add(() -> { // idle
            swapsController.setProgress(0.0f);
            swapsController.resetSwapDisplay();
        });
        STEPS.add(() -> { // order created (deposit QR)
            swapsController.moneroAddressTextField.setText(TESTNET_XMR_ADDRESS);
            if (!swapsController.sellersObservableList.isEmpty()) {
                swapsController.libp2pPeerTextField.setText(swapsController.sellersObservableList.get(0).multiaddr());
            } else {
                swapsController.libp2pPeerTextField.setText(SELLER_MULTIADDR);
                scheduleSellerRefresh();
            }
            swapsController.setProgress(-1.0f);
            swapsController.prepareSwapDisplay("Send between 0.001 BTC - 0.01 BTC", "waiting for deposit");
            swapsController.setSwapStatusDetail(TESTNET_BTC_ADDRESS, "You fully control this address");
            try {
                Image qr = new AddressQrCode(TESTNET_BTC_ADDRESS).generateBtc();
                swapsController.setSwapImage(qr, "Send BTC (you fully control this address)", true);
            } catch (Exception ignored) {
            }
        });
        STEPS.add(() -> { // btc received
            swapsController.setProgress(-1.0f);
            swapsController.setSwapStatus("Received Bitcoin - preparing swap...");
            swapsController.setSwapStep(1, "deposit received");
            swapsController.setSwapContextAmount(1_000_000L);
        });
        STEPS.add(() -> { // swap running
            swapsController.setProgress(-1.0f);
            swapsController.setLockIcon(SwapIconType.INITIATED);
            swapsController.setSwapStatus("Swap Running");
            swapsController.setSwapStatusDetail(SWAP_ID, "Swap ID");
            swapsController.setSwapStep(1, "negotiated");
        });
        STEPS.add(() -> { // btc locked
            swapsController.setProgress(1f / 15f);
            swapsController.setLockIcon(SwapIconType.BTC_LOCK);
            swapsController.setSwapStatus("BTC locked - waiting on confirmation & Alice's XMR");
            swapsController.setSwapStatusDetail(BTC_LOCK_TXID, "BTC TX");
            swapsController.setSwapStep(2, "confirming");
        });
        STEPS.add(() -> { // btc lock confirmed
            swapsController.setSwapStatus("BTC lock confirmed - waiting for Alice to lock XMR");
            swapsController.setSwapStep(2, "confirmed");
        });
        STEPS.add(() -> { // xmr lock proof
            swapsController.setProgress(2f / 15f);
            swapsController.setLockIcon(SwapIconType.XMR_LOCK);
            swapsController.setSwapStatus("XMR locked - waiting for 10 confirmations");
            swapsController.setSwapStatusDetail(XMR_LOCK_TXID, "XMR TX");
            swapsController.setSwapStep(3, "0/10 confs");
        });
        STEPS.add(() -> { // xmr confirmations mid
            swapsController.setProgress(5f / 15f);
            swapsController.setSwapStatus("XMR confirmations: 3/10");
            swapsController.setSwapStep(3, "3/10 confs");
        });
        STEPS.add(() -> { // xmr confirmations done
            swapsController.setProgress(12f / 15f);
            // as live: at 10/10 the wait for the seller's claim is step 4's
            swapsController.setSwapStatus("XMR final - waiting for Alice to redeem BTC");
            swapsController.setSwapStep(3, "10/10 confs");
            swapsController.setSwapStep(4, swap.gui.controller.pages.SwapStepLabels.WAITING_FOR_SELLER);
        });
        STEPS.add(() -> { // btc redeemed
            swapsController.setProgress(13f / 15f);
            swapsController.setLockIcon(SwapIconType.BTC_REDEEM);
            swapsController.setSwapStatus("BTC redeemed - redeeming XMR...");
            swapsController.setSwapStatusDetail(SWAP_ID, "Swap ID");
            swapsController.setSwapStep(4, "claimed");
            swapsController.setSwapStep(5, "preparing");
        });
        STEPS.add(() -> { // syncing xmr wallet
            swapsController.setProgress(-1.0f);
            swapsController.setLockIcon(SwapIconType.SYNCING);
            swapsController.setSwapStatus("Preparing Monero redeem...");
            swapsController.setSwapStep(5, "preparing redeem");
        });
        STEPS.add(() -> { // syncing heartbeat
            swapsController.setSwapStatus("Still preparing Monero redeem (30s)...");
        });
        STEPS.add(() -> { // sweeping
            swapsController.setProgress(14f / 15f);
            swapsController.setLockIcon(SwapIconType.SWEEPING);
            swapsController.setSwapStatus("Sweeping to your wallet...");
            swapsController.setSwapStep(5, "sweeping");
        });
        STEPS.add(() -> { // success
            swapsController.swapKillReset(false);
            swapsController.reserveSwapProgressSpace();
            swapsController.setSwapStatus("Swap Success", "#27be4d");
            swapsController.completeSwapSteps();
            swapsController.setLockIcon(SwapIconType.CHECK);
            swapsController.setSwapStatusDetail(SWAP_ID, "Swap ID");
            swapsController.showXmrReceived(125_178_240_000L, 121_760_000L);
        });
        STEPS.add(() -> { // refund pending (timelock armed; no tx yet)
            swapsController.swapKillReset(false);
            swapsController.setProgress(1f / 15f);
            swapsController.setLockIcon(SwapIconType.REFUND_PENDING);
            swapsController.setSwapStatus("Refund pending", "#be7327");
            swapsController.setSwapStatusDetail(SWAP_ID, "Waiting for the lock timelock; no cancel/refund transaction broadcast yet.");
            swapsController.setSwapStep(2, "confirmed");
        });
        STEPS.add(() -> { // cancel broadcast
            swapsController.swapKillReset(false);
            swapsController.setProgress(-1.0f);
            swapsController.setSwapStatus("Cancel broadcast", "#be7327");
            swapsController.setSwapStatusDetail(CANCEL_TXID, "Cancel transaction broadcast; waiting for confirmation before the refund.");
            swapsController.markSwapCanceledStep();
            swapsController.setLockIcon(SwapIconType.CANCEL_PENDING);
        });
        STEPS.add(() -> { // refund broadcast
            swapsController.swapKillReset(false);
            swapsController.setProgress(-1.0f);
            swapsController.setSwapStatus("Refund broadcast", "#be7327");
            swapsController.setSwapStatusDetail(REFUND_TXID, "Refund transaction broadcast; waiting for confirmation.");
            swapsController.markSwapRefundBroadcastStep();
            swapsController.setLockIcon(SwapIconType.REFUND_PENDING);
        });
        STEPS.add(() -> { // refunded
            swapsController.swapKillReset(false);
            swapsController.reserveSwapProgressSpace();
            swapsController.setSwapStatus("Swap refunded", "#be7327");
            swapsController.setSwapStatusDetail(SWAP_ID, "Swap ID");
            swapsController.markSwapRefundedStep();
            swapsController.failCurrentSwapStep();
            swapsController.setLockIcon(SwapIconType.CLOSE);
        });
        STEPS.add(() -> { // error
            swapsController.swapKillReset(false);
            swapsController.setSwapStatus("Swap Error", "#c0201a");
            swapsController.setSwapStatusDetail(SWAP_ID + ": RpcError", "Swap ID");
            swapsController.failCurrentSwapStep();
            swapsController.setLockIcon(SwapIconType.CLOSE);
        });
        STEPS.add(() -> { // failed: insufficient BTC (step 2)
            swapsController.swapKillReset(false);
            swapsController.setSwapStatus("Swap failed", "#c0201a");
            swapsController.setSwapStatusDetail("Not enough BTC for swap + fee.", "Swap ID");
            swapsController.setSwapStep(2, "confirming");
            swapsController.failCurrentSwapStep();
            swapsController.setLockIcon(SwapIconType.CLOSE);
        });
        STEPS.add(() -> { // failed: invalid seller signature (step 3)
            swapsController.swapKillReset(false);
            swapsController.setSwapStatus("Swap failed", "#c0201a");
            swapsController.setSwapStatusDetail("Invalid seller signature.", "Swap ID");
            swapsController.setSwapStep(3, "0/10 confs");
            swapsController.failCurrentSwapStep();
            swapsController.setLockIcon(SwapIconType.CLOSE);
        });
        STEPS.add(() -> { // failed: redeem wallet (step 5)
            swapsController.swapKillReset(false);
            swapsController.setSwapStatus("Swap failed", "#c0201a");
            swapsController.setSwapStatusDetail("Failed to redeem XMR: wallet RPC error", "Swap ID");
            swapsController.setSwapStep(5, "preparing");
            swapsController.failCurrentSwapStep();
            swapsController.setLockIcon(SwapIconType.CLOSE);
        });
        STEPS.add(() -> { // failed: long error (step 5)
            swapsController.swapKillReset(false);
            swapsController.setSwapStatus("Swap failed", "#c0201a");
            swapsController.setSwapStatusDetail("Unexpected swap error.", "Swap ID");
            swapsController.setSwapStep(5, "preparing");
            swapsController.failCurrentSwapStep();
            swapsController.setLockIcon(SwapIconType.CLOSE);
        });
        STEPS.add(() -> { // punished
            swapsController.swapKillReset(false);
            swapsController.setSwapStatus("Swap punished", "#c0201a");
            swapsController.setSwapStatusDetail("Refund window missed - BTC was claimed", "Swap ID");
            swapsController.setSwapStep(4, "timed out");
            swapsController.failCurrentSwapStep();
            swapsController.setLockIcon(SwapIconType.CLOSE);
        });
        STEPS.add(() -> { // failed: seller quote
            swapsController.swapKillReset(false);
            swapsController.setSwapStatus("Swap failed", "#c0201a");
            swapsController.setSwapStatusDetail("Seller quote failed: connection refused.", "Swap ID");
            swapsController.setSwapStep(1, "negotiated");
            swapsController.failCurrentSwapStep();
            swapsController.setLockIcon(SwapIconType.CLOSE);
        });
        STEPS.add(() -> { // failed: Monero wallet init
            swapsController.swapKillReset(false);
            swapsController.setSwapStatus("Swap failed", "#c0201a");
            swapsController.setSwapStatusDetail("Monero wallet RPC unavailable.", "Swap ID");
            swapsController.setSwapStep(1, "negotiated");
            swapsController.failCurrentSwapStep();
            swapsController.setLockIcon(SwapIconType.CLOSE);
        });
        STEPS.add(() -> { // failed: XMR finality
            swapsController.swapKillReset(false);
            swapsController.setSwapStatus("Swap failed", "#c0201a");
            swapsController.setSwapStatusDetail("Failed to wait for Monero lock finality.", "Swap ID");
            swapsController.setSwapStep(3, "0/10 confs");
            swapsController.failCurrentSwapStep();
            swapsController.setLockIcon(SwapIconType.CLOSE);
        });
        STEPS.add(() -> { // failed: XMR sweep timeout
            swapsController.swapKillReset(false);
            swapsController.setSwapStatus("Swap failed", "#c0201a");
            swapsController.setSwapStatusDetail("Failed to redeem XMR: sweep timed out.", "Swap ID");
            swapsController.setSwapStep(5, "sweeping");
            swapsController.failCurrentSwapStep();
            swapsController.setLockIcon(SwapIconType.CLOSE);
        });
        STEPS.add(() -> { // disconnected
            swapsController.swapKillReset(false);
            swapsController.setSwapStatus("Disconnected", "#c0201a");
            swapsController.setSwapStatusDetail("Disconnected from trade.", "Swap ID");
            swapsController.setSwapStep(3, "0/10 confs");
            swapsController.failCurrentSwapStep();
            swapsController.setLockIcon(SwapIconType.CLOSE);
        });
        STEPS.add(() -> { // aborted
            swapsController.swapKillReset(false);
            swapsController.setSwapStatus("Swap aborted", "#c0201a");
            swapsController.setSwapStatusDetail(SWAP_ID, "Swap ID");
            swapsController.failCurrentSwapStep();
            swapsController.setLockIcon(SwapIconType.CLOSE);
        });
        STEPS.add(() -> { // disconnected / reset
            swapsController.setProgress(0.0f);
            swapsController.resetSwapDisplay();
        });
    }
}
