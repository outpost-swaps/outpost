package swap.gui.controller;

import de.jensd.fx.glyphs.fontawesome.FontAwesomeIconView;
import javafx.fxml.FXML;
import javafx.fxml.FXMLLoader;
import javafx.scene.Parent;
import javafx.scene.control.Button;
import javafx.scene.control.Label;
import javafx.scene.control.Tooltip;
import javafx.scene.image.ImageView;
import javafx.scene.layout.AnchorPane;
import javafx.scene.layout.GridPane;
import javafx.scene.layout.HBox;
import javafx.scene.layout.VBox;
import javafx.scene.paint.Color;
import javafx.stage.Stage;
import org.fxmisc.flowless.VirtualizedScrollPane;
import org.fxmisc.richtext.InlineCssTextArea;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import swap.gui.GUISwap;
import swap.gui.controller.pages.*;
import swap.gui.controller.popups.PopupInformation;
import swap.gui.scene.MainScene;
import swap.helper.HelperProperties;
import swap.lib.AppSwap;
import swap.listener.AsbListener;
import swap.listener.StartupListener;
import swap.listener.SwapListener;
import swap.listener.impl.AsbListenerImpl;
import swap.listener.impl.SwapListenerImpl;
import swap.model.Changelog;
import swap.model.LogType;

import java.io.IOException;
import java.lang.invoke.MethodHandles;
import java.net.URL;
import java.time.Instant;
import java.time.ZoneOffset;
import java.time.format.DateTimeFormatter;
import java.util.ResourceBundle;

public class MainController extends BaseController implements StartupListener {
    public static AsbListener asbListener = null;
    public static SwapListener swapListener = null;
    private static MainController instance = null;
    private final Logger log = LoggerFactory.getLogger(MethodHandles.lookup().lookupClass());

    @FXML
    public GridPane mainPane;
    @FXML
    public Button swapsNav;
    @FXML
    public Button liquidityNav;
    @FXML
    public Button historyNav;
    @FXML
    public Button withdrawNav;
    @FXML
    public Button settingsNav;
    @FXML
    public AnchorPane contentPane;
    @FXML
    public ImageView whirlpoolImage;
    @FXML
    public FontAwesomeIconView asbIcon;
    @FXML
    public FontAwesomeIconView dojoStatusIcon;
    @FXML
    public Label navSpotPriceText;
    @FXML
    public HBox navSpotPriceChip;

    public void setNavSpotPrice(String text) {
        updateGui(() -> {
            navSpotPriceText.setText(text);
            if (!navSpotPriceChip.isVisible())
                navSpotPriceChip.setVisible(true);
        });
    }
    private String passphrase = "";
    private String pairingPayload = "";
    private boolean startAsb;
    @FXML
    private SwapsController swapsController;
    @FXML
    private GridPane swapsView;
    @FXML
    private HistoryController historyController;
    @FXML
    private GridPane historyView;
    @FXML
    private GridPane withdrawView;
    @FXML
    private LiquidityController liquidityController;
    @FXML
    private GridPane liquidityView;
    @FXML
    private SettingsController settingsController;
    @FXML
    private WithdrawController withdrawController;
    @FXML
    private GridPane settingsView;
    @FXML
    public VBox logsContainer;
    @FXML
    public VirtualizedScrollPane<InlineCssTextArea> vScrollPane;
    @FXML
    public InlineCssTextArea logs;

    public static MainController getInstance() {
        return instance;
    }

    public void setDojoStatus(String status, boolean reachable, boolean checking) {
        updateGui(() -> {
            if (dojoStatusIcon == null) return;
            dojoStatusIcon.setVisible(true);
            dojoStatusIcon.setFill(reachable ? Color.LIGHTGREEN : checking ? Color.ORANGE : Color.SALMON);
            javafx.scene.control.Tooltip.install(dojoStatusIcon,
                    new javafx.scene.control.Tooltip(reachable ? "Dojo Connected"
                            : checking ? "Dojo Connecting" : status.equals("not configured")
                            ? "Dojo Not Configured" : "Dojo Unreachable"));
        });
    }

    private volatile boolean asbRunning;
    private volatile boolean asbInitialized;
    private volatile boolean asbListed;

    public void setAsbRunning(boolean running) {
        asbRunning = running;
        if (!running) {
            asbInitialized = false;
            asbListed = false;
        }
        refreshAsbIndicator();
    }

    public void setAsbInitialized() {
        asbInitialized = true;
        refreshAsbIndicator();
    }

    public void setAsbListed(boolean listed) {
        asbListed = listed;
        refreshAsbIndicator();
    }

    private void refreshAsbIndicator() {
        updateGui(() -> {
            if (asbIcon == null) return;
            boolean show = asbRunning && asbInitialized;
            asbIcon.setVisible(show);
            asbIcon.setManaged(show);
            String status = !asbRunning ? "ASB Stopped"
                    : !asbInitialized ? "ASB Starting"
                    : asbListed ? "ASB Listed" : "ASB Running";
            javafx.scene.control.Tooltip.install(asbIcon, new javafx.scene.control.Tooltip(status));
        });
    }

    public static MainScene create(Stage stage) {
        FXMLLoader swapFxmlLoader = new FXMLLoader(GUISwap.class.getResource("/main-gui-view.fxml"));
        try {
            Parent root = swapFxmlLoader.load();
            MainController mainController = swapFxmlLoader.getController();
            MainScene mainScene = new MainScene(mainController, root, stage.getScene().getWidth(), stage.getScene().getHeight());
            GUISwap.applyTheme(mainScene);
            return mainScene;
        } catch (IOException e) {
            throw new RuntimeException(e);
        }
    }

    void configure(String passphrase, String pairingPayload, boolean startAsb) {
        this.passphrase = passphrase;
        this.pairingPayload = pairingPayload;
        this.startAsb = startAsb;
        registerClient();
    }

    @Override
    public void initialize(URL location, ResourceBundle resources) {
        instance = this;
        Tooltip.install(navSpotPriceChip, new Tooltip("XMR spot price"));
        installLogContextMenu();
        initializeSwaps();
        initializeResume();
        initializeAsb();
//        ClientWhirlpool.copyWhirlpoolClientFilesToTempLocation();
        startWhirlpoolStatusIndicator();
        super.initialize(location, resources);
    }

    /**
     * RichTextFX areas have no context menu of their own (a JavaFX TextArea does), so right-click
     * on the log did nothing. Copy the selection, copy everything, or select all.
     */
    private void installLogContextMenu() {
        if (logs == null) return;
        javafx.scene.control.MenuItem copy = new javafx.scene.control.MenuItem("Copy");
        copy.setOnAction(event -> logs.copy());
        javafx.scene.control.MenuItem copyAll = new javafx.scene.control.MenuItem("Copy all");
        copyAll.setOnAction(event -> {
            javafx.scene.input.ClipboardContent content = new javafx.scene.input.ClipboardContent();
            content.putString(logs.getText());
            javafx.scene.input.Clipboard.getSystemClipboard().setContent(content);
        });
        javafx.scene.control.MenuItem selectAll = new javafx.scene.control.MenuItem("Select all");
        selectAll.setOnAction(event -> logs.selectAll());
        javafx.scene.control.ContextMenu menu = new javafx.scene.control.ContextMenu(copy, copyAll, selectAll);
        // "Copy" only makes sense with something selected
        menu.setOnShowing(event -> copy.setDisable(logs.getSelectedText().isEmpty()));
        logs.setContextMenu(menu);
    }

    /**
     * Purely observational: just reads MixingService's already-maintained isAvailable()
     * flag (a plain boolean check, no I/O) and GUISwap.isAsbRunning() every few seconds to
     * toggle the top-bar icon. Mixing being down/disabled/never-started just means this
     * stays hidden - it can never block or affect swaps, matching MixingService's own
     * decoupling invariant (see its class javadoc).
     */
    private void startWhirlpoolStatusIndicator() {
        GUISwap.scheduledExecutorService.scheduleAtFixedRate(() -> {
            boolean mixingConnected = swap.mixing.MixingService.getInstance() != null
                    && swap.mixing.MixingService.getInstance().isAvailable();
            boolean show = GUISwap.isAsbRunning() && mixingConnected;
            updateGui(() -> {
                if (whirlpoolImage != null) {
                    whirlpoolImage.setVisible(show);
                    whirlpoolImage.setManaged(show);
                    if (show) {
                        javafx.scene.control.Tooltip.install(whirlpoolImage, new javafx.scene.control.Tooltip("Whirlpool mixing connected"));
                    }
                }
            });
        }, 5, 5, java.util.concurrent.TimeUnit.SECONDS);
    }

    @Override
    public void onClientStarted() {
        this.printSwapLogLn(LogType.INFO, "Swap client has started.", false);
//        startWhirlpool(false);
        startAsb();
        this.historyController.refreshHistoryList();
        this.swapsController.onClientStarted();
        preloadWithdrawPage();
        GUISwap.appSwap.maybeAutoResume(historyController);

        // Look for more Electrum servers through Dojo Bay in the background; they only join the lists after they pass a probe.
        try {
            java.io.File cache = new java.io.File(swap.lib.AppSwap.getSwapRootDir(), "dojo-directory-cache.json");
            swap.helper.HelperElectrumDiscovery.startInBackground(
                    swap.helper.HelperNetwork.isTestnet(GUISwap.appSwap.getParams()), cache,
                    () -> updateGui(() -> {
                        SettingsController settings = SettingsController.getInstance();
                        if (settings != null) {
                            settings.refreshElectrumServers();
                        }
                    }));
        } catch (Throwable ignored) {
            // discovery is a convenience; the curated list always works
        }

        // Warn at once if the saved Electrum server or Monero node no longer answers.
        try {
            swap.helper.HelperSavedServerCheck.startInBackground(
                    swap.helper.HelperNetwork.isTestnet(GUISwap.appSwap.getParams()),
                    HelperProperties.electrumServer, HelperProperties.moneroDaemon,
                    line -> updateGui(() -> this.printSwapLogLn(LogType.INFO, line, false)));
        } catch (Throwable ignored) {
            // a convenience check
        }

        boolean hasSeenPopup = HelperProperties.hasSeenUpdatePopup;
        if(!hasSeenPopup) {
            GUISwap.executorService.submit(() -> {
                try {
                    HelperProperties.setProperty(HelperProperties.KEY_HAS_SEEN_UPDATE_POPUP, "true");
                    updateGui(() -> PopupInformation.create("What's New", "What's New", Changelog.TEXT));
                } catch (Exception e) {
                    throw new RuntimeException(e);
                }
            });
        }
    }

    @Override
    protected void setupListeners() {
        asbListener = new AsbListenerImpl(this, swapsController, liquidityController);
        swapListener = new SwapListenerImpl(this, swapsController, historyController);
    }

    @Override
    public Button getStartButton() {
        return null;
    }

    private void initializeSwaps() {
        swapsPage();
    }

    private void initializeResume() {
        try {
            FXMLLoader swapsFxmlLoader = new FXMLLoader(GUISwap.class.getResource("/pages/history.fxml"));
            historyView = swapsFxmlLoader.load();
            historyController = swapsFxmlLoader.getController();
        } catch (Exception e) {
            log.error("Failed to initialize HistoryController for Resume Swaps; ", e);
        }
    }

    private void initializeAsb() {
        try {
            FXMLLoader swapsFxmlLoader = new FXMLLoader(GUISwap.class.getResource("/pages/liquidity.fxml"));
            liquidityView = swapsFxmlLoader.load();
            liquidityController = swapsFxmlLoader.getController();
        } catch (Exception e) {
            log.error("Failed to initialize LiquidityController for ASB; ", e);
        }
    }

    /**
     * Load the Withdraw view a little after the client starts so its balance,
     * UTXO, and Dojo fee fetches are already in flight before the user clicks
     * the tab. Delaying the FX-thread load avoids competing with the swap
     * client's startup requests; the controller's own 10-second summary warm-up
     * then runs before the tab is normally opened.
     */
    private void preloadWithdrawPage() {
        GUISwap.scheduledExecutorService.schedule(
                () -> updateGui(this::ensureWithdrawLoaded),
                2, java.util.concurrent.TimeUnit.SECONDS);
    }

    /** Must be called on the FX thread. */
    private void ensureWithdrawLoaded() {
        if (withdrawController != null) return;
        try {
            FXMLLoader withdrawFxmlLoader = new FXMLLoader(GUISwap.class.getResource("/pages/withdraw.fxml"));
            withdrawView = withdrawFxmlLoader.load();
            withdrawController = withdrawFxmlLoader.getController();
        } catch (IOException e) {
            log.error("Failed to preload withdraw page: withdraw.fxml;", e);
        }
    }


    @FXML
    public void swapsPage() {
        try {
            contentPane.getChildren().clear();
            if (swapsController == null) {
                FXMLLoader swapsFxmlLoader = new FXMLLoader(GUISwap.class.getResource("/pages/swaps.fxml"));
                swapsView = swapsFxmlLoader.load();
                swapsController = swapsFxmlLoader.getController();
            }
            contentPane.getChildren().add(swapsView);
            swapsNav.setDisable(true);
            liquidityNav.setDisable(false);
            historyNav.setDisable(false);
            withdrawNav.setDisable(false);
            settingsNav.setDisable(false);

            swapsView.requestFocus();

        } catch (IOException e) {
            log.error("Failed to load swaps page: swaps.fxml;", e);
        }
    }

    @FXML
    public void historyPage() {
        contentPane.getChildren().clear();
        if (historyController == null) // should have been initialized in initializeResume()
            initializeResume();
            contentPane.getChildren().add(historyView);

        this.historyController.refreshHistoryList();

        swapsNav.setDisable(false);
        historyNav.setDisable(true);
        liquidityNav.setDisable(false);
        settingsNav.setDisable(false);
        withdrawNav.setDisable(false);
        historyView.requestFocus();
    }

    @FXML
    public void liquidityPage() {
        contentPane.getChildren().clear();
        if (liquidityController == null) // should have been initialized in initializeAsb()
            initializeAsb();
            contentPane.getChildren().add(liquidityView);
            liquidityController.updateAsbState();

        swapsNav.setDisable(false);
        historyNav.setDisable(false);
        liquidityNav.setDisable(true);
        settingsNav.setDisable(false);
        withdrawNav.setDisable(false);
        liquidityView.requestFocus();
    }

    // TODO: Temporarily disabled until fixed
    @FXML
    public void withdrawPage() {
        contentPane.getChildren().clear();
        ensureWithdrawLoaded();
        if (withdrawView == null || withdrawController == null) return;
        contentPane.getChildren().add(withdrawView);
        withdrawController.onPageShown();

        swapsNav.setDisable(false);
        historyNav.setDisable(false);
        liquidityNav.setDisable(false);
        settingsNav.setDisable(false);
        withdrawNav.setDisable(true);
        withdrawView.requestFocus();
    }

    @FXML
    public void settingsPage() {
        try {
            contentPane.getChildren().clear();
            if (settingsController == null) {
                FXMLLoader swapsFxmlLoader = new FXMLLoader(GUISwap.class.getResource("/pages/settings.fxml"));
                settingsView = swapsFxmlLoader.load();
                settingsController = swapsFxmlLoader.getController();
            }
            contentPane.getChildren().add(settingsView);

            swapsNav.setDisable(false);
            historyNav.setDisable(false);
            liquidityNav.setDisable(false);
            settingsNav.setDisable(true);
            withdrawNav.setDisable(false);
            settingsView.requestFocus();
        } catch (IOException e) {
            log.error("Failed to load settings page: settings.fxml;", e);
        }
    }

    /******** CLIENT INITIALIZATIONS ********/
    private void registerClient() {
        // the opening lines are written by logClientOpening() once the main window is on screen:
        // written here, they carried the time Connect was pressed, before the start screen's download
        try {
            if (HelperProperties.hasPropertiesFile()) {
                // Has properties file, and user has entered their passphrase
                GUISwap.appSwap = new AppSwap(passphrase, this);

            } else if (!HelperProperties.hasPropertiesFile() && !pairingPayload.isEmpty()) {
                // Does not have properties file, but user has entered payload and passphrase
                GUISwap.appSwap = new AppSwap(passphrase, this);
            }
        } catch (Exception e) {
            throw new RuntimeException("Failed to register client", e);
        }
    }

    /**
     * The first lines of the swap log, written when the main window opens. {@code rpcSetupMillis} is
     * how long the start screen spent checking (and on a first launch downloading) the Monero wallet
     * tools, so the log explains the time between pressing Connect and the window opening.
     */
    public void logClientOpening(long rpcSetupMillis) {
        printSwapLogLn(LogType.INFO, "Privacy is a human fight. Free Samourai. Free TDev.", false);
        if (rpcSetupMillis >= 5_000) {
            printSwapLogLn(LogType.INFO, rpcDownloadLogLine(rpcSetupMillis), false);
        }
        printSwapLogLn(LogType.INFO, "Initializing client...", false);
    }

    /** "Downloaded and verified the Monero wallet tools in 2 min 6 s (first launch only)." */
    static String rpcDownloadLogLine(long millis) {
        long seconds = millis / 1000;
        String took = seconds < 60 ? seconds + " s" : (seconds / 60) + " min " + (seconds % 60) + " s";
        return "Downloaded and verified the Monero wallet tools in " + took + " (first launch only).";
    }

    public void startAsb() {
        if (startAsb) {
            GUISwap.appAsb.start();
            this.liquidityController.prepareAsb();
        }
    }

    /**
     * Starts the ASB from inside the running app (the Liquidity page button), the same start the
     * unlock screen's "Start ASB" box triggers. It blocks for a while (the Monero wallet RPC has to
     * come up), so it runs off the UI thread; a failure is logged and the panel returns to its button.
     */
    public void startAsbNow() {
        startAsb = true;
        updateGui(() -> liquidityController.prepareAsb());
        GUISwap.executorService.submit(() -> {
            try {
                GUISwap.appAsb.start();
            } catch (Exception e) {
                printSwapLogLn(LogType.WARN, "[ASB] Could not start: " + e.getMessage(), false);
                liquidityController.onAsbStartFailed();
            }
        });
    }

    public SwapsController getSwapsController() {
        return swapsController;
    }

    public HistoryController getHistoryController() {
        return historyController;
    }


    /***** LOGS CONTROLLER STUFF *****/
    public void printSwapLogLn(LogType logType, String message, boolean kill) {
        String time = Instant.parse(Instant.now().toString())
                .atOffset(ZoneOffset.UTC).format(DateTimeFormatter.ofPattern("yyyy-MM-dd HH:mm:ss"));
        String logMessage = time + " :: " + message;
        String textColor = "-fx-fill: " + logType.getColor() + ";";

        updateGui(() -> {
            // Color exactly the paragraphs this message occupies. A message with
            // embedded newlines spans several paragraphs, so a per-message
            // counter drifts and shifts every later color (see LogParagraphs).
            boolean areaWasEmpty = logs.getLength() == 0;
            int[] range = LogParagraphs.appendedRange(logs.getParagraphs().size(), areaWasEmpty, logMessage);
            logs.appendText(areaWasEmpty ? logMessage : "\n" + logMessage);
            for (int paragraph = range[0]; paragraph <= range[1]; paragraph++) {
                logs.setStyle(paragraph, textColor);
            }
            logs.requestFollowCaret(); // scroll to bottom
        });

        if (kill) {
            endSwapThread();
        }
    }

    // TODO: Re-add Whirlpool with new client
}
