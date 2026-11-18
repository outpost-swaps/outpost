package swap.gui.controller;

import javafx.application.Platform;
import javafx.fxml.FXML;
import javafx.scene.control.*;
import javafx.scene.image.ImageView;
import javafx.scene.input.KeyCode;
import javafx.scene.layout.HBox;
import javafx.scene.layout.VBox;
import javafx.stage.Stage;
import swap.gui.GUISwap;
import swap.gui.scene.MainScene;
import swap.helper.*;
import swap.lib.AppPriceTicker;
import swap.lib.AppSwap;
import swap.lib.AppXmrRpc;
import swap.listener.RpcDownloadListener;
import swap.model.LogType;
import swap.model.ScreenType;

import java.io.File;
import java.util.concurrent.atomic.AtomicBoolean;

public class PairingController extends BaseController {
    public static RpcDownloadListener rpcDownloadListener = null;
    public static AtomicBoolean pairing = new AtomicBoolean(false);
    @FXML
    public ImageView swapsLogo;
    @FXML
    public PasswordField walletPassphraseTextField;
    @FXML
    public Button pairingButton;
    @FXML
    public Button connectButton;
    @FXML
    public Button createButton;
    @FXML
    public javafx.scene.layout.VBox pairRow;
    @FXML
    public VBox startButtonsRow;
    @FXML
    public javafx.scene.layout.VBox formBox;
    @FXML
    public VBox stageBox;
    @FXML
    public Label stageCheck;
    @FXML
    public Label stageSetup;
    @FXML
    public Label stageDownload;
    @FXML
    public Label stageOpen;
    @FXML
    public Label startText;
    @FXML
    public ProgressBar rpcDownloadProgress;
    @FXML
    public CheckBox startAsbCheckbox;
    @FXML
    private Stage stage;

    private static final String[] STAGE_TEXT = {
            "Checking your passphrase",
            "Setting up your wallet",
            "Downloading Monero wallet files",
            "Opening your wallet"
    };

    /** Marks stage {@code active} (0-based) as current, the ones before it done and the rest pending; -1 hides the list. */
    static String stageLine(int index, int active) {
        return stageLine(index, active, -1);
    }

    /** As above; the download step also shows its percentage once it is known. */
    static String stageLine(int index, int active, int downloadPercent) {
        String mark = index < active ? "\u2713" : index == active ? "\u25CF" : "\u25CB";
        String suffix = index == 2 && index == active && downloadPercent >= 0 ? "  \u00B7  " + Math.min(100, downloadPercent) + "%" : "";
        return mark + "  " + STAGE_TEXT[index] + suffix;
    }

    /** A first launch shows the Connect button; while pairing runs it gives way to the step list. */
    private boolean firstLaunch;

    /** True once real download figures have arrived: until then the bar just shows that work is going on. */
    private volatile boolean downloadProgressSeen;
    private volatile int downloadPercent = -1;

    /**
     * Whether the step list and bar are shown. A first connect has a long setup, so it shows them.
     * Unlocking a wallet that is already connected takes seconds and must stay on the unlock screen
     * (a setup-style screen there is confusing); only a real download, which takes a while, brings the
     * list up.
     */
    static boolean showsStepList(boolean firstLaunch, boolean downloadProgressSeen, int active) {
        return active >= 0 && (firstLaunch || downloadProgressSeen);
    }

    private void showStage(int active) {
        updateGui(() -> {
            if (active >= 0 && !showsStepList(firstLaunch, downloadProgressSeen, active)) {
                showStartNote("Unlocking...");
            }
            boolean show = showsStepList(firstLaunch, downloadProgressSeen, active);
            stageBox.setVisible(show);
            stageBox.setManaged(show);
            // the progress takes the form's place (same spot, fixed size): the page does not move
            formBox.setVisible(!show);
            rpcDownloadProgress.setVisible(show);
            rpcDownloadProgress.setManaged(show);
            if (!show) {
                return;
            }
            startText.setText(null);
            // Setting up, downloading and opening take a while: the bar is there for all of it, moving
            // without a figure until the download reports a real percentage.
            if (!(active == 2 && downloadProgressSeen)) {
                rpcDownloadProgress.setProgress(ProgressBar.INDETERMINATE_PROGRESS);
            }
            Label[] labels = {stageCheck, stageSetup, stageDownload, stageOpen};
            for (int i = 0; i < labels.length; i++) {
                labels[i].setText(stageLine(i, active, downloadPercent));
                labels[i].setTextFill(javafx.scene.paint.Color.web(i < active ? "#919191" : i == active ? "#ffffff" : "#4c4c4c"));
            }
        });
    }

    /** While pairing runs nothing on the screen can be changed; on a failure it all comes back. */
    private void setBusy(boolean busy) {
        updateGui(() -> {
            walletPassphraseTextField.setDisable(busy);
            // Only ever disable here: they are enabled again by the Tor check, once Tor is ready and no pairing runs.
            if (busy) {
                connectButton.setDisable(true);
                createButton.setDisable(true);
            }
            if (firstLaunch) {
                show(connectButton, !busy);
                show(createButton, !busy);
                show(startButtonsRow, !busy);
            }
            startAsbCheckbox.setDisable(busy);
            pairingButton.setDisable(busy);
            if (!busy) {
                showStage(-1);
            }
        });
    }

    @Override
    protected ScreenType getScreenType() {
        return ScreenType.PAIRING;
    }

    @Override
    public Button getStartButton() {
        return pairingButton;
    }

    /** Connect wallet and Create new wallet begin pairing, so they wait for Tor like the Pair button does. */
    @Override
    protected java.util.List<Button> getExtraStartButtons() {
        return java.util.List.of(connectButton, createButton);
    }

    @Override
    protected void setupListeners() {
    }

    public void init(Stage stage) {
        this.stage = stage;
        rpcDownloadProgress.setVisible(false);
        rpcDownloadProgress.setManaged(false);
        rpcDownloadListener = new RpcDownloadListener() {
            @Override
            public void onRpcDownloaded() {
                downloadProgressSeen = false;
                downloadPercent = -1;
                updateGui(() -> startText.setText(null));
            }

            @Override
            public void onRpcDownloadError(String error) {
                updateGui(() -> showStartError("Error downloading RPC: " + error));
                setBusy(false);
                GUISwap instance = GUISwap.getInstance();
                if (instance != null) {
                    try {
                        instance.stop();
                    } catch (Exception e) {
                        e.printStackTrace();
                    }
                }
            }

            @Override
            public void onRpcDownloadProgress(long pct) {
                downloadProgressSeen = true;
                downloadPercent = (int) pct;
                showStage(2); // the percentage is part of the step line
                updateGui(() -> rpcDownloadProgress.setProgress(Math.min(1.0d, pct / 99d)));
            }
        };

        // A first launch has nothing to unlock yet: one button opens the guided window. A wallet that
        // is already connected only needs its passphrase.
        boolean returning = HelperProperties.hasPropertiesFile();
        firstLaunch = !returning;
        show(connectButton, !returning);
        show(createButton, !returning);
        show(startButtonsRow, !returning); // an empty row still takes a gap in the column
        // invisible, not removed: on the first screen they still hold their space, so the logo and the
        // first control are in the same place as on the unlock screen
        walletPassphraseTextField.setVisible(returning);
        pairRow.setVisible(returning);
        if (returning) {
            pairingButton.setText("Unlock Wallet");
            walletPassphraseTextField.setOnKeyReleased(e -> {
                if (e.getCode() == KeyCode.ENTER) {
                    if (!pairingButton.isDisable()) {
                        onPairingButtonClick();
                    }
                }
            });
        }

        Tooltip logoTooltip = new Tooltip("Asking permission is seeking denial");
        logoTooltip.setStyle("-fx-font-size: 24;");
        Tooltip.install(swapsLogo, logoTooltip);
        walletPassphraseTextField.setTooltip(new Tooltip("Your wallet passphrase, or the app password you set."));
    }

    /** The line under the form: a problem, in red. It has a fixed height, so nothing moves. */
    private void showStartError(String text) {
        startText.setTextFill(javafx.scene.paint.Color.web("#e5534b"));
        startText.setText(text);
    }

    /** The line under the form: progress, in grey. */
    private void showStartNote(String text) {
        startText.setTextFill(javafx.scene.paint.Color.web("#9aa0a6"));
        startText.setText(text);
    }

    private static void show(javafx.scene.Node node, boolean visible) {
        node.setVisible(visible);
        node.setManaged(visible);
    }

    /** A first launch: the guided window brings the wallet in (payload, backup file or seed words), then pairing starts. */
    @FXML
    public void onConnectClick() {
        swap.gui.controller.popups.PopupConnectWallet.show(stage, false)
                .ifPresent(result -> startPairing(result.pairingPayload(), result.passphrase(), result.startAsb(), result.servers()));
    }

    /** A first launch without a wallet: the same window makes a new one (12 words + passphrase, as Ashigaru does). */
    @FXML
    public void onCreateClick() {
        swap.gui.controller.popups.PopupConnectWallet.show(stage, true)
                .ifPresent(result -> startPairing(result.pairingPayload(), result.passphrase(), result.startAsb(), result.servers()));
    }

    /** A wallet that is already connected: unlock it with its passphrase. */
    @FXML
    public void onPairingButtonClick() {
        startPairing("", walletPassphraseTextField.getText(), startAsbCheckbox.isSelected(), null);
    }

    private void startPairing(String pairingPayload, String passphrase, boolean startAsb, swap.helper.HelperServerChoice.Choice servers) {
        startText.setText(null);
        if (!HelperProperties.hasPropertiesFile() && pairingPayload.isEmpty()) {
            showStartError("Connect a wallet first.");
            return;
        }
        if (passphrase.isEmpty()) {
            showStartError("Enter your wallet passphrase.");
            return;
        }

        pairing.set(true);
        setBusy(true);
        showStage(0);
        try {
            HelperProperties.init(pairingPayload);
            // the servers picked in the window, saved before the app starts so its first start uses them
            swap.helper.HelperServerChoice.save(servers);
        } catch (Exception e) {
            updateGui(() -> showStartError("Error: " + e.getMessage()));
            pairing.set(false);
            setBusy(false);
            return;
        }
        GUISwap.executorService.submit(() -> {
            boolean isValid = HelperWallet.checkPassphrase(HelperProperties.mnemonicEncrypted, passphrase);
            if (!isValid) {
                updateGui(() -> showStartError("Wrong passphrase."));
                pairing.set(false);
                setBusy(false);
                return;
            }

            showStage(1);
            MainScene swapsScene;
            try {
                swapsScene = MainController.create(stage);
                swapsScene.getMainController().configure(passphrase, pairingPayload, startAsb);
            } catch (Exception e) {
                updateGui(() -> showStartError("Error: " + e.getMessage()));
                pairing.set(false);
                setBusy(false);
                return;
            }

            AppSwap appSwap = GUISwap.appSwap;
            if (appSwap == null) {
                updateGui(() -> showStartError("Error: appSwap is null"));
                pairing.set(false);
                setBusy(false);
                return;
            }
            showStage(2);
            long rpcSetupStarted = System.currentTimeMillis();
            appSwap.getProxy().ifPresentOrElse(proxy -> maybeDownloadXmrRpcAndSetDir(proxy, appSwap.getProxyPort()), () -> {});
            swapsScene.getMainController().logClientOpening(System.currentTimeMillis() - rpcSetupStarted);
            showStage(3);
            MainScene finalSwapsScene = swapsScene;
            updateGui(() -> {
                stage.setScene(finalSwapsScene);
                Platform.runLater(() -> finalSwapsScene.getRoot().requestFocus());
            });

            pairing.set(false);
            appSwap.start();
        });
    }

    public void maybeDownloadXmrRpcAndSetDir(String proxy, int proxyPort) {
        File rpcRootDir = AppXmrRpc.maybeDownloadXmrRpc(proxy, rpcDownloadListener);
        GUISwap.appSwap.rpcRootDir = rpcRootDir;
        if (GUISwap.appPriceTicker != null) {
            GUISwap.appPriceTicker.stop();
        }
        GUISwap.appPriceTicker = new AppPriceTicker(rpcRootDir, proxyPort);
        GUISwap.appPriceTicker.start();
    }
}