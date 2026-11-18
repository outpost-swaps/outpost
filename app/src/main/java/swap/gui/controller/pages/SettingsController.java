package swap.gui.controller.pages;

import com.samourai.wallet.hd.AddressType;
import com.samourai.whirlpool.client.wallet.beans.WhirlpoolAccount;
import javafx.beans.property.SimpleStringProperty;
import javafx.collections.FXCollections;
import javafx.collections.ObservableList;
import javafx.event.ActionEvent;
import javafx.fxml.FXML;
import javafx.geometry.Insets;
import javafx.scene.Scene;
import javafx.scene.control.Button;
import javafx.scene.control.Label;
import javafx.scene.control.TextField;
import javafx.scene.control.*;
import javafx.scene.layout.GridPane;
import javafx.scene.layout.VBox;
import javafx.scene.paint.Color;
import javafx.scene.text.Font;
import javafx.stage.Stage;
import org.bitcoinj.core.Coin;
import org.bitcoinj.params.MainNetParams;
import org.json.JSONObject;
import swap.client.ClientSwap;
import swap.gui.GUISwap;
import swap.gui.controller.BaseController;
import swap.gui.controller.MainController;
import swap.gui.controller.popups.PopupAddNode;
import swap.helper.*;
import swap.lib.AppAsb;
import swap.lib.AppSwap;
import swap.model.LogType;
import swap.model.Multiaddr;
import java.awt.*;
import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.net.URI;
import java.net.URL;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.time.Duration;
import java.util.List;
import java.util.*;

public class SettingsController extends BaseController {
    private static SettingsController instance = null;
    @FXML
    public GridPane settingsPane;
    @FXML
    public TextField network;
    @FXML
    public TextField proxy;
    @FXML
    public TextField asbMinBtc;
    @FXML
    public TextField asbMaxBtc;
    @FXML
    public TextField asbManualRate;
    @FXML
    Spinner<Double> asbRateFee;
    @FXML
    public Button updateButton;
    // one Save button for the ASB Trading card: it saves whichever group changed (the old separate
    // Apply and Update buttons were easy to mix up)
    private boolean tradingChanged;
    private boolean mixingChanged;
    @FXML
    Button rendezvousButton;
    @FXML
    ComboBox<String> xmrNode;
    @FXML
    ComboBox<String> electrumServer;
    @FXML
    CheckBox autoTx0;
    @FXML
    CheckBox trustDaemon;
    @FXML
    TextField dojoField;
    @FXML
    Button testDojoButton;
    @FXML
    RadioButton ownMoneroNode;
    @FXML
    RadioButton publicMoneroNode;
    @FXML
    RadioButton ownElectrumServer;
    @FXML
    RadioButton publicElectrumServer;
    @FXML
    ToggleGroup electrumServerType;
    @FXML
    ToggleGroup moneroNodeType;
    /** True while the options are set for display only, so the change is not saved. */
    private boolean showingNodeTypeDefaults;
    @FXML
    CheckBox daemonFailover;

    @FXML
    CheckBox electrumFailover;
    @FXML
    ComboBox<String> poolSize;
    String minBtc = null;
    String maxBtc = null;
    String rateFee = null;
    String manualRateInit = null;
    private ObservableList<Multiaddr> rendezvousObservableList = null;

    public static SettingsController getInstance() {
        return instance;
    }

    @Override
    public void initialize(URL url, ResourceBundle rb) {
        super.initialize(url, rb);
        instance = this;
        AppSwap appSwap = GUISwap.appSwap;
        if (appSwap == null) return;
        network.setText(appSwap.getParams() == MainNetParams.get() ? "Mainnet" : "Testnet/Stagenet");
        appSwap.getProxy().ifPresentOrElse(proxyString -> proxy.setText(proxyString), () -> {});
        setupMoneroDaemons();
        setupElectrumServers();
        setupAsbConfigs();
        setupWhirlpoolConfigs();
        getRendezvousPeers();

    }

    @Override
    public Button getStartButton() {
        return null;
    }

    @Override
    protected void setupListeners() {
    }

    private void setupMoneroDaemons() {
        ObservableList<String> moneroDaemons = FXCollections.observableArrayList(GUISwap.appSwap.getMoneroDaemonUrls());
        xmrNode.setItems(moneroDaemons);

        String moneroDaemon = GUISwap.appSwap.getMoneroDaemon();
        if (!moneroDaemons.contains(moneroDaemon)) // checks if saved node is a default option
            moneroDaemons.add(moneroDaemon);
        xmrNode.setValue(moneroDaemon);

        xmrNode.valueProperty().addListener((observable, oldValue, newValue) -> resetTestButton(testMoneroButton));
        xmrNode.valueProperty().addListener((observable, oldValue, newValue) -> {
            if (newValue != null) {
                if (oldValue == null)
                    setXmrNode(newValue);
                else if (!newValue.equals(oldValue))
                    setXmrNode(newValue);
            }
        });
    }

    public void setXmrNode(String url) {
        GUISwap.appSwap.setMoneroDaemon(url);
        MainController.getInstance().printSwapLogLn(LogType.INFO, "Shutting down Monero Wallet RPC.", false);
        for (ClientSwap clientSwap : GUISwap.appSwap.getSwapClients().values()) {
            clientSwap.restartXmrRpcProcess();
        }
        MainController.getInstance().printSwapLogLn(
                LogType.HIGHLIGHT,
                "New Monero Wallet RPC instances started with daemon " + url + ".",
                GUISwap.isSwapRunning()
        );

        if (GUISwap.isAsbRunning()) {
            AppAsb appAsb = GUISwap.appAsb;
            if(appAsb != null) appAsb.restart();
        }
    }

    public void addXmrNode(String url) {
        if (!GUISwap.appSwap.getMoneroDaemonUrls().contains(url))
            xmrNode.getItems().add(url);
        xmrNode.setValue(url); // triggers event listener
    }

    private void setupElectrumServers() {
        ObservableList<String> electrumServers = FXCollections.observableArrayList(GUISwap.appSwap.getElectrumServerUrls());
        electrumServer.setItems(electrumServers);

        String electrumServerUrl = GUISwap.appSwap.getElectrumServer();
        if (!electrumServers.contains(electrumServerUrl)) // checks if saved electrum server is a default option
            electrumServers.add(electrumServerUrl);
        electrumServer.setValue(electrumServerUrl);

        electrumServer.valueProperty().addListener((observable, oldValue, newValue) -> resetTestButton(testElectrumButton));
        electrumServer.valueProperty().addListener((observable, oldValue, newValue) -> {
            if (refreshingElectrumList) {
                return; // refilling the list is not a choice of server
            }
            if (newValue != null) {
                if (oldValue == null)
                    setElectrumServer(newValue);
                else if (!newValue.equals(oldValue))
                    setElectrumServer(newValue);
            }
        });
    }

    private boolean refreshingElectrumList;

    /**
     * Discovery finished: refill the Electrum list with the servers that passed. The selection stays
     * where it was and, above all, nothing is switched (a switch restarts the ASB).
     */
    public void refreshElectrumServers() {
        String selected = electrumServer.getValue();
        refreshingElectrumList = true;
        try {
            electrumServer.getItems().setAll(GUISwap.appSwap.getElectrumServerUrls());
            if (selected != null && !electrumServer.getItems().contains(selected)) {
                electrumServer.getItems().add(selected);
            }
            electrumServer.setValue(selected);
        } finally {
            refreshingElectrumList = false;
        }
    }

    public void setElectrumServer(String url) {
        GUISwap.appSwap.setElectrumServer(url);
        MainController.getInstance().printSwapLogLn(LogType.HIGHLIGHT, "Using new Electrum server: " + url + ".", GUISwap.isSwapRunning());

        if (GUISwap.isAsbRunning()) {
            AppAsb appAsb = GUISwap.appAsb;
            if(appAsb != null) appAsb.restart();
        }
    }

    public void addElectrumServer(String url) {
        if (!GUISwap.appSwap.getElectrumServerUrls().contains(url))
            electrumServer.getItems().add(url);
        electrumServer.setValue(url); // triggers event listener
    }

    private void setupAsbConfigs() {
        minBtc = AppAsb.getMinQuantity().toPlainString();
        maxBtc = AppAsb.getMaxQuantity().toPlainString();
        rateFee = String.format("%.2f", AppAsb.getFee());
        String savedManualRate = HelperProperties.getManualRate();
        manualRateInit = savedManualRate == null ? "" : savedManualRate;

        SpinnerValueFactory<Double> valueFactory = new SpinnerValueFactory.DoubleSpinnerValueFactory(-10.00, 20.00, Double.parseDouble(rateFee), 0.5);
        asbRateFee.setValueFactory(valueFactory);

        updateGui(() -> {
            asbMinBtc.setText(minBtc);
            asbMaxBtc.setText(maxBtc);
            asbManualRate.setText(manualRateInit);
        });

        asbMinBtc.textProperty().addListener((observable, oldValue, newValue) -> {
            if (!newValue.equals(minBtc)) {
                tradingChanged = true; updateButton.setDisable(false);
            }
        });
        asbMaxBtc.textProperty().addListener((observable, oldValue, newValue) -> {
            if (!newValue.equals(maxBtc)) {
                tradingChanged = true; updateButton.setDisable(false);
            }
        });
        asbManualRate.textProperty().addListener((observable, oldValue, newValue) -> {
            if (!newValue.trim().equals(manualRateInit)) {
                tradingChanged = true; updateButton.setDisable(false);
            }
        });
        asbRateFee.valueProperty().addListener((observable, oldValue, newValue) -> {
            if (!newValue.equals(Double.parseDouble(rateFee))) {
                tradingChanged = true; updateButton.setDisable(false);
            }
        });
    }

    @FXML
    public void onUpdateButtonClick() {
        String min = asbMinBtc.getText().trim();
        String max = asbMaxBtc.getText().trim();
        String rate = asbRateFee.getValue().toString().trim();
        String manual = asbManualRate.getText().trim();

        GUISwap.executorService.submit(() -> {
            if (!isNumeric(min)) {
                MainController.getInstance().printSwapLogLn(LogType.WARN, "ASB Min BTC value entered is not a number: " + min, false);
            } else if (Double.parseDouble(min) < 0.0005) {
                MainController.getInstance().printSwapLogLn(LogType.WARN, "ASB Min BTC is too low (" + min +"). Please set minimum above 0.0005 BTC.", false);
            } else if (!isNumeric(max)) {
                MainController.getInstance().printSwapLogLn(LogType.WARN, "ASB Max BTC value entered is not a number: " + max, false);
            } else if (!manual.isEmpty() && !isValidManualRate(manual)) {
                MainController.getInstance().printSwapLogLn(LogType.WARN, "ASB Manual Rate is not a valid BTC amount per 1 XMR: " + manual + ". Leave empty to track the market.", false);
            } else {
                // update configs
                try {
                    HelperProperties.update(props -> {
                        if (!min.equals(minBtc)) {
                            props.setProperty("minQuantity", min);
                            minBtc = min;
                        }
                        if (!max.equals(maxBtc)) {
                            props.setProperty("maxQuantity", max);
                            maxBtc = max;
                        }
                        if (!rate.equals(rateFee)) {
                            props.setProperty("fee", rate);
                            rateFee = rate;
                        }
                        if (!manual.equals(manualRateInit)) {
                            props.setProperty("manualRate", manual);
                            manualRateInit = manual;
                        }
                    });
                } catch (IOException e) {
                    throw new RuntimeException(e);
                }

                String rateFeeDisplay = manual.isEmpty()
                        ? "Rate Fee = " + rate
                        : "Rate Fee = " + rate + " (inactive — recalculated from the manual rate)";
                String manualDisplay = manual.isEmpty()
                        ? "off (market rate + Rate Fee)"
                        : manual + " BTC/XMR (advertised as-is)";
                String restartNote = GUISwap.isAsbRunning() ? "; restarting ASB to apply" : "";
                MainController.getInstance().printSwapLogLn(LogType.HIGHLIGHT,
                        "Updated ASB Configs: Min BTC = " + min + "; Max BTC = " + max + "; " + rateFeeDisplay
                                + "; Manual Rate = " + manualDisplay + restartNote + ";",
                        GUISwap.isSwapRunning());

                AppAsb.setMinQuantity(Coin.parseCoin(min));
                AppAsb.setMaxQuantity(Coin.parseCoin(max));
                AppAsb.setFee(Float.parseFloat(rate));
                AppAsb.setManualPrice(manual.isEmpty() ? null : Coin.parseCoin(manual));

                updateGui(() -> {
                    tradingChanged = false;
                    updateButton.setDisable(!mixingChanged);
                });

                if (GUISwap.isAsbRunning()) {
                    AppAsb appAsb = GUISwap.appAsb;
                    if(appAsb != null) appAsb.restart();
                }
            }
        });
    }

    private boolean isValidManualRate(String str) {
        try {
            return Coin.parseCoin(str).value > 0;
        } catch (Exception e) {
            return false;
        }
    }

    private boolean isNumeric(String str) {
        try {
            Float.parseFloat(str);
            return true;
        } catch (NumberFormatException e) {
            return false;
        }
    }

    private void setupWhirlpoolConfigs() {
        String currentDojo = HelperProperties.dojoUrl;
        dojoField.setText(currentDojo == null ? "" : currentDojo);
        dojoField.setTooltip(new Tooltip("The Dojo your balances and fee estimates come from. Change... switches it after the next start."));
        autoTx0.setSelected(HelperProperties.isAutoTx0());
        autoTx0.setTooltip(new Tooltip(
                "Automatically Tx0 new ASB swap payouts into a Whirlpool\n"
                        + "coinjoin using the pool size selected below."));
        trustDaemon.setSelected(HelperProperties.isTrustRemoteDaemon());
        trustDaemon.setTooltip(new Tooltip(
                "Faster Monero sync from remote mainnet daemons (larger batches\n"
                        + "over Tor). Outputs stay fully verified; the daemon sees your\n"
                        + "sync pattern. Test networks and localhost are always trusted."));

        poolSize.setItems(FXCollections.observableArrayList("0.025btc", "0.25btc"));
        String currentPoolSize = HelperProperties.getPoolSize();
        boolean savedValueStillValid = List.of("0.025btc", "0.25btc").contains(currentPoolSize);
        poolSize.setValue(savedValueStillValid ? currentPoolSize : "0.025btc");

        autoTx0.selectedProperty().addListener((obs, oldVal, newVal) -> { mixingChanged = true; updateButton.setDisable(false); });
        trustDaemon.selectedProperty().addListener((obs, oldVal, newVal) -> {
            if (!showingNodeTypeDefaults) {
                saveTakesEffectNextStart("trustRemoteDaemon", newVal, "Trust Daemon");
            }
        });
        publicMoneroNode.setTooltip(new Tooltip(
                "A public node reached over Tor. Swaps ask other independent nodes to\n"
                        + "confirm the seller's lock. Daemon Failover and Trust Daemon apply."));
        ownMoneroNode.setTooltip(new Tooltip(
                "The selected node is yours. It is pinned (no failover to public nodes),\n"
                        + "trusted for fast sync, and trusted alone for the seller's lock check."));
        daemonFailover.setSelected(HelperProperties.isDaemonFailover());
        daemonFailover.setTooltip(new Tooltip(
                "Auto-switch to the next healthy Monero daemon when the active\n"
                        + "one drops. Off = the configured daemon is PINNED (no\n"
                        + "switching), curated default or not. Takes effect next start."));
        daemonFailover.selectedProperty().addListener((obs, oldVal, newVal) -> {
            if (!showingNodeTypeDefaults) {
                saveTakesEffectNextStart("daemonFailover", newVal, "Daemon Failover");
            }
        });
        moneroNodeType().install();
        electrumFailover.setSelected(HelperProperties.isElectrumFailover());
        electrumFailover.setTooltip(new Tooltip(
                "Auto-switch to the next healthy electrum server when the active\n"
                        + "one drops. Off = the configured server is PINNED (no\n"
                        + "switching), curated default or not. Takes effect next start."));
        electrumFailover.selectedProperty().addListener((obs, oldVal, newVal) -> {
            if (!showingNodeTypeDefaults) {
                saveTakesEffectNextStart("electrumFailover", newVal, "Electrum Failover");
            }
        });
        publicElectrumServer.setTooltip(new Tooltip(
                "A public Electrum server reached over Tor. Every server you use sees\n"
                        + "all of your wallet's addresses. Electrum Failover applies."));
        ownElectrumServer.setTooltip(new Tooltip(
                "The selected server is yours. It is pinned: no failover to public\n"
                        + "servers, which would see all of your wallet's addresses."));
        electrumServerType().install();
        poolSize.valueProperty().addListener((obs, oldVal, newVal) -> { mixingChanged = true; updateButton.setDisable(false); });
    }

    private NodeTypeSelector moneroSelector;
    private NodeTypeSelector electrumSelector;

    private NodeTypeSelector moneroNodeType() {
        if (moneroSelector == null) {
            moneroSelector = new NodeTypeSelector(xmrNode, ownMoneroNode, publicMoneroNode,
                    "moneroOwnNodes", "moneroLastOwnNode", "moneroLastPublicNode", "moneroNodeIsOwn",
                    this::applyMoneroNodeType, () -> openAddMoneroNode(true), url -> "");
        }
        return moneroSelector;
    }

    private NodeTypeSelector electrumServerType() {
        if (electrumSelector == null) {
            electrumSelector = new NodeTypeSelector(electrumServer, ownElectrumServer, publicElectrumServer,
                    "electrumOwnServers", "electrumLastOwnServer", "electrumLastPublicServer", "electrumServerIsOwn",
                    this::applyElectrumServerType, () -> openAddElectrumNode(true),
                    url -> swap.helper.HelperElectrumDiscovery.isDiscovered(url) ? "   (Dojo Bay)" : "");
        }
        return electrumSelector;
    }

    /** An own Electrum server is pinned: show failover off (greyed) without saving it. */
    static boolean shownElectrumFailover(boolean ownServer, boolean savedFailover) {
        return !ownServer && savedFailover;
    }

    private void applyElectrumServerType(boolean ownServer) {
        showingNodeTypeDefaults = true;
        try {
            electrumFailover.setSelected(shownElectrumFailover(ownServer, HelperProperties.isElectrumFailover()));
        } finally {
            showingNodeTypeDefaults = false;
        }
        electrumFailover.setDisable(ownServer);
    }

    /** What the two public-node options show: an own node is pinned (no failover) and trusted. */
    static boolean[] shownNodeOptions(boolean ownNode, boolean savedFailover, boolean savedTrust) {
        return ownNode ? new boolean[] {false, true} : new boolean[] {savedFailover, savedTrust};
    }

    /**
     * The public-node options mean nothing for the user's own node, which is pinned and trusted:
     * show that (greyed) without saving it, so the saved choices come back for a public node.
     */
    private void applyMoneroNodeType(boolean ownNode) {
        boolean[] shown = shownNodeOptions(ownNode, HelperProperties.isDaemonFailover(), HelperProperties.isTrustRemoteDaemon());
        showingNodeTypeDefaults = true;
        try {
            daemonFailover.setSelected(shown[0]);
            trustDaemon.setSelected(shown[1]);
        } finally {
            showingNodeTypeDefaults = false;
        }
        daemonFailover.setDisable(ownNode);
        trustDaemon.setDisable(ownNode);
    }

    private void saveTakesEffectNextStart(String key, boolean value, String label) {
        GUISwap.executorService.submit(() -> {
            try {
                HelperProperties.setProperty(key, String.valueOf(value));
                MainController.getInstance().printSwapLogLn(LogType.HIGHLIGHT,
                        "[ASB] " + label + " saved: " + (value ? "enabled" : "disabled") + ". Takes effect next time the ASB or a swap starts.",
                        false);
            } catch (IOException e) {
                MainController.getInstance().printSwapLogLn(LogType.WARN, "Failed to save " + label + ": " + e.getMessage(), false);
            }
        });
    }

    /** The card's one Save: the trading values and/or the mixing options, whichever changed. */
    @FXML
    public void onSaveAsbSettingsClick() {
        if (tradingChanged) {
            onUpdateButtonClick();
        }
        if (mixingChanged) {
            onApplyButtonClick();
        }
    }

    @FXML
    public void onApplyButtonClick() {
        boolean enabled = autoTx0.isSelected();
        String selectedPoolSize = poolSize.getValue();

        GUISwap.executorService.submit(() -> {
            try {
                HelperProperties.setAutoTx0(enabled);
                if (selectedPoolSize != null && !selectedPoolSize.isBlank()) {
                    HelperProperties.setPoolSize(selectedPoolSize.trim());
                }
            } catch (IOException e) {
                MainController.getInstance().printSwapLogLn(LogType.WARN, "Failed to save mixing settings: " + e.getMessage(), false);
                return;
            }

            updateGui(() -> { mixingChanged = false; updateButton.setDisable(!tradingChanged); });

            if (enabled) {
                String dojoUrl = HelperProperties.dojoUrl;
                if (dojoUrl == null || dojoUrl.isEmpty()) {
                    MainController.getInstance().printSwapLogLn(LogType.WARN, "Cannot enable mixing: no Dojo configured.", false);
                    return;
                }
                swap.wallet.SwapsWallet swapsWallet = swap.wallet.SwapsWallet.getInstance();
                if (swapsWallet != null) {
                    swap.mixing.MixingService.enable(swapsWallet.getHdWallet(), GUISwap.appSwap.getParams(), dojoUrl, HelperProperties.dojoApiKey);
                }
                MainController.getInstance().printSwapLogLn(LogType.HIGHLIGHT, "[MIXING] Auto Tx0 enabled.", false);
            } else {
                swap.mixing.MixingService.disable();
                MainController.getInstance().printSwapLogLn(LogType.HIGHLIGHT, "[MIXING] Auto Tx0 disabled.", false);
            }
        });
    }

    private void getRendezvousPeers() {
        rendezvousObservableList = FXCollections.observableArrayList(GUISwap.appSwap.getRendezvousPeers());
        updateRendezvousSummary();
        rendezvousObservableList.addListener((javafx.collections.ListChangeListener<Multiaddr>) c -> updateRendezvousSummary());
    }

    private void updateRendezvousSummary() {
        int n = rendezvousObservableList == null ? 0 : rendezvousObservableList.size();
        rendezvousButton.setTooltip(new Tooltip(n + (n == 1 ? " rendezvous peer" : " rendezvous peers")));
    }

    /** Rendezvous peers are rarely touched, so the table lives in its own window instead of a tile. */
    @FXML
    private void onShowRendezvousPeersClick(ActionEvent event) {
        TableView<Multiaddr> table = new TableView<>(rendezvousObservableList);
        table.setColumnResizePolicy(TableView.CONSTRAINED_RESIZE_POLICY_FLEX_LAST_COLUMN);
        TableColumn<Multiaddr, String> addressColumn = new TableColumn<>("Address");
        addressColumn.setCellValueFactory(p -> new SimpleStringProperty(p.getValue().address()));
        addressColumn.setPrefWidth(280);
        TableColumn<Multiaddr, String> portColumn = new TableColumn<>("Port");
        portColumn.setCellValueFactory(p -> new SimpleStringProperty(String.valueOf(p.getValue().port())));
        portColumn.setPrefWidth(70);
        TableColumn<Multiaddr, String> peerColumn = new TableColumn<>("PeerId");
        peerColumn.setCellValueFactory(p -> new SimpleStringProperty(p.getValue().peerId() == null ? "-" : p.getValue().peerId()));
        peerColumn.setPrefWidth(340);
        table.getColumns().addAll(addressColumn, portColumn, peerColumn);
        Label title = new Label("Rendezvous Peers");
        title.getStyleClass().add("settings-card-title");
        Button add = new Button("Add Peer");
        add.setPrefWidth(132);
        add.setOnAction(this::onClickAddRendezvousNode);
        javafx.scene.layout.Region spacer = new javafx.scene.layout.Region();
        javafx.scene.layout.HBox.setHgrow(spacer, javafx.scene.layout.Priority.ALWAYS);
        javafx.scene.layout.HBox header = new javafx.scene.layout.HBox(12, title, spacer, add);
        header.setAlignment(javafx.geometry.Pos.CENTER_LEFT);
        javafx.scene.layout.VBox box = new javafx.scene.layout.VBox(10, header, table);
        box.setPadding(new javafx.geometry.Insets(14));
        javafx.scene.layout.VBox.setVgrow(table, javafx.scene.layout.Priority.ALWAYS);
        javafx.scene.Scene scene = new javafx.scene.Scene(box, 780, 420, javafx.scene.paint.Color.web("#0e0e0e"));
        GUISwap.applyTheme(scene);
        javafx.stage.Stage stage = new javafx.stage.Stage();
        stage.setTitle("Rendezvous Peers");
        stage.setScene(scene);
        stage.show();
    }

    public void addRendezvousPeer(Multiaddr multiaddr) {
        try {
            if (!rendezvousObservableList.contains(multiaddr))
                rendezvousObservableList.add(multiaddr);
        } catch (Exception e) {
            e.printStackTrace();
        }
    }

    public void openSwapDir(ActionEvent actionEvent) {
        GUISwap.executorService.submit(() -> {
            File appRootDir = AppSwap.getSwapRootDir();
            try {
                Desktop.getDesktop().open(appRootDir);
            } catch (Exception e) {
                String osName = System.getProperty("os.name");
                if (osName.toLowerCase().contains("nux") || osName.toLowerCase().contains("nix")) {
                    try {
                        Runtime.getRuntime().exec(
                                new String[]{"sh", "-c", "xdg-open '" + appRootDir.getAbsolutePath() + "'"}
                        );
                    } catch (Exception e1) {
                        MainController.getInstance().printSwapLogLn(LogType.ERROR, "Failed to open data directory: " + e1.getMessage(), false);
                    }
                } else if (osName.toLowerCase().contains("mac")) {
                    try {
                        Runtime.getRuntime().exec(
                                new String[]{"sh", "-c", "open '" + appRootDir.getAbsolutePath() + "'"}
                        );
                    } catch (Exception e1) {
                        MainController.getInstance().printSwapLogLn(LogType.ERROR, "Failed to open data directory: " + e1.getMessage(), false);
                    }
                } else if (osName.toLowerCase().contains("windows")) {
                    try {
                        Runtime.getRuntime().exec(new String[]
                                {"rundll32", "url.dll,FileProtocolHandler",
                                        appRootDir.getAbsolutePath()});
                    } catch (Exception e1) {
                        MainController.getInstance().printSwapLogLn(LogType.ERROR, "Failed to open data directory: " + e1.getMessage(), false);
                    }
                } else {
                    MainController.getInstance().printSwapLogLn(LogType.ERROR, "Failed to open data directory: " + e.getMessage(), false);
                }
            }
        });
    }

    @FXML
    private void onClickAddElectrumNode(ActionEvent actionEvent) {
        openAddElectrumNode(false);
    }

    private void openAddElectrumNode(boolean ownPreset) {
        PopupAddNode.create("Add Electrum Server", "tcp://..., ssl://...", true, true, ownPreset, (address, port, own) -> {
            String truncatedAddress = address;
            if(truncatedAddress.contains(":")) {
                String[] splitAddress = truncatedAddress.split(":");
                truncatedAddress = splitAddress[0];
            }
            String electrumNodeUrl = truncatedAddress + ":" + port;
            SettingsController settingsController = SettingsController.getInstance();
            if(settingsController != null) {
                settingsController.electrumServerType().mark(electrumNodeUrl, own);
                settingsController.addElectrumServer(electrumNodeUrl);
                HelperBtcNodesProperties.getInstance().addCustomNode(electrumNodeUrl);
            }
        });
    }

    @FXML
    private void onClickAddMoneroNode(ActionEvent actionEvent) {
        openAddMoneroNode(false);
    }

    private void openAddMoneroNode(boolean ownPreset) {
        PopupAddNode.create("Add Monero Node", "http://...", true, true, ownPreset, (address, port, own) -> {
            String truncatedAddress = address;
            if(truncatedAddress.contains(":")) {
                String[] splitAddress = truncatedAddress.split(":");
                truncatedAddress = splitAddress[0];
            }
            String xmrNodeUrl = truncatedAddress + ":" + port;
            SettingsController settingsController = SettingsController.getInstance();
            if(settingsController != null) {
                settingsController.moneroNodeType().mark(xmrNodeUrl, own);
                settingsController.addXmrNode(xmrNodeUrl);
                HelperXmrNodesProperties.getInstance().addCustomNode(xmrNodeUrl);
            }
        });
    }

    @FXML
    private void onClickAddRendezvousNode(ActionEvent actionEvent) {
        PopupAddNode.create("Add Rendezvous Peer", "/ip4/..., /dns4/..., /onion3/...", false, (address, port) -> {
            if(port != -1) return;
            Multiaddr multiaddr = HelperAddress.parseMultiaddr(address);
            SettingsController settingsController = SettingsController.getInstance();
            if(settingsController != null) {
                settingsController.addRendezvousPeer(multiaddr);
                HelperRendezvousPeersProperties.getInstance().addCustomPeer(multiaddr.toString());
            }
        });
    }

    // The ASB XMR wallet ("asb-wallet") is created by monero-wallet-rpc from a
    // random 25-word mnemonic. The words are the only way to restore the
    // seller's XMR if the wallet files are lost, so expose them read-only.
    // The wallet is only open while the ASB runs; query_key needs it open.
    @FXML
    private void onShowAsbXmrSeedClick(ActionEvent actionEvent) {
        askPassphraseThen("ASB Monero Seed", this::showAsbXmrSeed);
    }

    private void showAsbXmrSeed() {
        String endpoint = GUISwap.appAsb != null ? GUISwap.appAsb.getXmrRpcEndpoint() : null;
        if (endpoint == null) {
            showSeedPopup(null, "The ASB is not running. Start the ASB first to view its Monero wallet seed.");
            return;
        }
        GUISwap.executorService.submit(() -> {
            String mnemonic = null;
            String error = null;
            try {
                mnemonic = fetchWalletMnemonic(endpoint);
            } catch (Exception e) {
                error = e.getMessage();
            }
            String finalMnemonic = mnemonic;
            String finalError = error;
            updateGui(() -> {
                if (finalMnemonic != null) {
                    showSeedPopup(finalMnemonic, null);
                } else {
                    showSeedPopup(null, "Could not read the ASB Monero wallet seed: " + finalError);
                }
            });
        });
    }

    // Account ZPUBs live here now instead of the Withdraw page: they're static
    // account metadata for recovery/verification, not part of the withdrawal
    // flow. Shown in a themed popup like the other info windows.
    @FXML
    private Button testElectrumButton;
    @FXML
    private Button testMoneroButton;

    /** Checks the selected Electrum server over Tor: right chain, answering (same check as setup). */
    @FXML
    private void onTestElectrumClick(ActionEvent event) {
        String url = electrumServer.getValue();
        boolean testnet = swap.helper.HelperNetwork.isTestnet(GUISwap.appSwap.getParams());
        runServerTest(url, testElectrumButton, "Electrum server", port -> {
            swap.helper.HelperElectrumProbe.Result r = swap.helper.HelperElectrumProbe.probe(url, testnet,
                    swap.helper.HelperElectrumProbe.throughTor(port), 60_000);
            return new String[] {String.valueOf(r.ok()), r.ok() ? "Connected (" + seconds(r.millis()) + ", block " + r.height() + ")" : r.reason()};
        });
    }

    /** Checks the selected Monero node over Tor: answering, synced, right network (same check as setup). */
    @FXML
    private void onTestMoneroClick(ActionEvent event) {
        String url = xmrNode.getValue();
        boolean stagenet = swap.helper.HelperNetwork.isTestnet(GUISwap.appSwap.getParams());
        runServerTest(url, testMoneroButton, "Monero node", port -> {
            swap.helper.HelperMoneroProbe.Result r = swap.helper.HelperMoneroProbe.probe(url, stagenet,
                    swap.helper.HelperElectrumProbe.throughTor(port), 60_000);
            return new String[] {String.valueOf(r.ok()), r.ok() ? "Connected (" + seconds(r.millis()) + ", block " + r.height() + ")" : r.reason()};
        });
    }

    private static String seconds(long millis) {
        return String.format(java.util.Locale.ROOT, "%.1f s", millis / 1000.0);
    }

    /**
     * Runs one server test off the UI thread. The button itself shows the state (there is no room for
     * a result line in these rows): "Testing... 5 s", then "\u2713 2.1 s" in green or "\u2717 No answer"
     * in red, with the details in its tooltip and in the swap log. Clicking it again tests again; a
     * different server selected puts it back to "Test connection".
     */
    private void runServerTest(String url, Button button, String what, java.util.function.IntFunction<String[]> test) {
        if (url == null || url.isBlank()) {
            return;
        }
        button.setDisable(true);
        button.setStyle("");
        button.setTooltip(null);
        long started = System.currentTimeMillis();
        javafx.animation.Timeline ticker = new javafx.animation.Timeline(new javafx.animation.KeyFrame(
                javafx.util.Duration.seconds(1),
                e -> button.setText("Testing... " + (System.currentTimeMillis() - started) / 1000 + " s")));
        ticker.setCycleCount(javafx.animation.Animation.INDEFINITE);
        button.setText("Testing... 0 s");
        ticker.play();
        Thread thread = new Thread(() -> {
            String[] outcome;
            Integer port = HelperTorPort.waitForSocksPort(10_000);
            if (port == null) {
                outcome = new String[] {"false", "Tor is not ready yet", ""};
            } else {
                try {
                    outcome = test.apply(port);
                } catch (Exception e) {
                    outcome = new String[] {"false", e.getMessage() == null ? "no answer" : e.getMessage(), ""};
                }
            }
            boolean ok = Boolean.parseBoolean(outcome[0]);
            String detail = outcome[1];
            String seconds = String.format(java.util.Locale.ROOT, "%.1f s", (System.currentTimeMillis() - started) / 1000.0);
            javafx.application.Platform.runLater(() -> {
                ticker.stop();
                button.setDisable(false);
                button.setText(ok ? "\u2713 " + seconds : "timed out".equals(detail) ? "\u2717 No answer" : "\u2717 Failed");
                button.setStyle("-fx-font-weight: bold; -fx-text-fill: " + (ok ? "#3fb950" : "#e5534b") + ";");
                button.setTooltip(new Tooltip(url + "\n" + (ok ? detail : "Did not pass: " + detail)));
                MainController.getInstance().printSwapLogLn(ok ? LogType.SUCCESS : LogType.WARN,
                        what + " " + url + (ok ? ": " + detail + "." : " test failed: " + detail + "."), false);
            });
        }, "settings-server-test");
        thread.setDaemon(true);
        thread.start();
    }

    /** A different server was selected: the last result no longer applies. */
    private static void resetTestButton(Button button) {
        if (button != null && !button.isDisable()) {
            button.setText("Test connection");
            button.setStyle("");
            button.setTooltip(null);
        }
    }

    /** Logs in to the Dojo in use over Tor; the button shows the result, like the Electrum and Monero tests. */
    @FXML
    private void onTestDojoClick(ActionEvent event) {
        String url = HelperProperties.dojoUrl;
        String key = HelperProperties.dojoApiKey;
        if (url == null || url.isEmpty() || key == null) {
            testDojoButton.setText("No Dojo yet");
            return;
        }
        runServerTest(url, testDojoButton, "Dojo", port -> {
            HelperDojoLogin.Result result = HelperDojoLogin.test(url, key, port);
            String detail = result.ok() ? "logged in" : switch (result.outcome()) {
                case BAD_KEY -> "the API key was rejected";
                case UNREACHABLE -> "could not reach it over Tor";
                default -> "unexpected reply";
            };
            return new String[] {String.valueOf(result.ok()), detail};
        });
    }

    /** Settings > Wallet > Dojo: pick another Dojo; it is used after the next start. */
    @FXML
    private void onChangeDojoClick(ActionEvent event) {
        javafx.stage.Window owner = event != null && event.getSource() instanceof javafx.scene.Node node && node.getScene() != null
                ? node.getScene().getWindow() : null;
        swap.gui.controller.popups.PopupChangeDojo.show(owner).ifPresent(url ->
                {
                    dojoField.setText(url + "   (after restart)");
                    MainController.getInstance().printSwapLogLn(LogType.HIGHLIGHT,
                            "Dojo saved: " + url + ". Restart Outpost to use it.", false);
                });
    }

    @FXML
    private void onShowAccountZpubsClick(ActionEvent actionEvent) {
        String swapsDepositPub = GUISwap.appSwap.getWalletSupplier().getWallet(WhirlpoolAccount.SWAPS_DEPOSIT, AddressType.SEGWIT_NATIVE).getPub();
        String swapsRefundsPub = GUISwap.appSwap.getWalletSupplier().getWallet(WhirlpoolAccount.SWAPS_REFUNDS, AddressType.SEGWIT_NATIVE).getPub();
        String swapsAsbPub = GUISwap.appSwap.getWalletSupplier().getWallet(WhirlpoolAccount.ASB, AddressType.SEGWIT_NATIVE).getPub();

        String[][] keys = {
                {"SWAPS DEPOSIT ZPUB", swapsDepositPub},
                {"SWAPS REFUNDS ZPUB", swapsRefundsPub},
                {"SWAPS ASB ZPUB", swapsAsbPub},
        };
        updateGui(() -> showZpubsWindow(keys));
    }

    private static void showZpubsWindow(String[][] keys) {
        {
            VBox box = dialogRoot("BTC Account ZPUBs",
                    "Public keys only: they show addresses and balances but cannot spend.");
            for (String[] key : keys) {
                VBox card = new VBox(6);
                card.getStyleClass().add("app-card");
                Label name = new Label(key[0]);
                name.getStyleClass().add("dialog-section-title");
                TextField field = new TextField(key[1]);
                field.setEditable(false);
                field.getStyleClass().add("import-mono-small");
                javafx.scene.control.Button copy = new javafx.scene.control.Button("Copy");
                copy.getStyleClass().add("dialog-button-secondary");
                copy.setMinWidth(72);
                copy.setPrefWidth(72);
                copy.setOnAction(e -> {
                    javafx.scene.input.ClipboardContent content = new javafx.scene.input.ClipboardContent();
                    content.putString(key[1]);
                    javafx.scene.input.Clipboard.getSystemClipboard().setContent(content);
                });
                javafx.scene.layout.HBox row = new javafx.scene.layout.HBox(8, field, copy);
                row.setAlignment(javafx.geometry.Pos.CENTER_LEFT);
                javafx.scene.layout.HBox.setHgrow(field, javafx.scene.layout.Priority.ALWAYS);
                card.getChildren().addAll(name, row);
                box.getChildren().add(card);
            }
            showDialog("BTC Account ZPUBs", box);
        }
    }

    /** The shared look of the Settings info windows: big red title, grey hint, room for cards. */
    private static VBox dialogRoot(String title, String hint) {
        VBox box = new VBox(14);
        box.setPrefWidth(620);
        box.setPadding(new Insets(22, 28, 22, 28));
        box.setStyle("-fx-background-color: #0e0e0e;");
        VBox head = new VBox(4);
        Label titleLabel = new Label(title);
        titleLabel.setTextFill(Color.web("#c0201a"));
        titleLabel.setFont(Font.font("Arial Bold", 24));
        head.getChildren().add(titleLabel);
        if (hint != null) {
            Label hintLabel = new Label(hint);
            hintLabel.getStyleClass().add("import-hint");
            hintLabel.setWrapText(true);
            head.getChildren().add(hintLabel);
        }
        box.getChildren().add(head);
        return box;
    }

    /** Adds the Close button row, then shows the window. */
    private static void showDialog(String title, VBox box) {
        Stage stage = new Stage();
        javafx.scene.control.Button close = new javafx.scene.control.Button("Close");
        close.getStyleClass().add("dialog-button-secondary");
        close.setMinWidth(72);
        close.setPrefWidth(72);
        close.setCancelButton(true);
        close.setDefaultButton(true);
        close.setOnAction(e -> stage.close());
        javafx.scene.layout.HBox buttons = new javafx.scene.layout.HBox(close);
        buttons.setAlignment(javafx.geometry.Pos.CENTER_RIGHT);
        // Same right inset as the cards' padding, so Close lines up with the Copy buttons above it.
        buttons.setPadding(new Insets(0, 10, 0, 0));
        box.getChildren().add(buttons);
        stage.setTitle(title);
        Scene scene = new Scene(box, Color.web("#0e0e0e"));
        GUISwap.applyTheme(scene);
        stage.setScene(scene);
        stage.setResizable(false);
        stage.show();
        stage.toFront();
        stage.setAlwaysOnTop(true);
    }

    private String fetchWalletMnemonic(String endpoint) throws Exception {
        // monero-wallet-rpc runs with --rpc-login, so it demands HTTP Digest auth.
        // The credentials are embedded in the endpoint URL, which HttpClient
        // ignores: pull them out and answer the 401 challenge ourselves.
        URI full = URI.create(endpoint);
        String userInfo = full.getUserInfo();
        URI target = new URI(full.getScheme(), null, full.getHost(), full.getPort(), full.getPath(), null, null);
        String user = userInfo == null ? "" : userInfo.substring(0, Math.max(userInfo.indexOf(':'), 0));
        String password = userInfo == null || !userInfo.contains(":") ? "" : userInfo.substring(userInfo.indexOf(':') + 1);

        HttpClient client = HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(5)).build();
        String requestBody = "{\"jsonrpc\":\"2.0\",\"id\":\"0\",\"method\":\"query_key\",\"params\":{\"key_type\":\"mnemonic\"}}";
        HttpResponse<String> response = postJson(client, target, requestBody, null);
        if (response.statusCode() == 401) {
            java.util.Map<String, String> challenge = swap.helper.HelperDigestAuth.parseChallenge(
                    response.headers().firstValue("WWW-Authenticate").orElse(null));
            if (challenge.isEmpty()) {
                throw new RuntimeException("the wallet RPC refused the request (HTTP 401) without a digest challenge");
            }
            String cnonce = Long.toHexString(new java.security.SecureRandom().nextLong());
            String authorization = swap.helper.HelperDigestAuth.authorization(
                    user, password, "POST", target.getRawPath(), challenge, cnonce, 1);
            response = postJson(client, target, requestBody, authorization);
        }
        if (response.statusCode() != 200) {
            throw new RuntimeException("the wallet RPC answered HTTP " + response.statusCode());
        }
        JSONObject json = new JSONObject(response.body());
        if (json.has("error")) {
            JSONObject rpcError = json.getJSONObject("error");
            throw new RuntimeException(rpcError.optString("message", "wallet RPC error"));
        }
        return json.getJSONObject("result").getString("key");
    }

    private static HttpResponse<String> postJson(HttpClient client, URI target, String body, String authorization) throws Exception {
        HttpRequest.Builder builder = HttpRequest.newBuilder(target)
                .timeout(Duration.ofSeconds(10))
                .header("Content-Type", "application/json")
                .POST(HttpRequest.BodyPublishers.ofString(body));
        if (authorization != null) {
            builder.header("Authorization", authorization);
        }
        return client.send(builder.build(), HttpResponse.BodyHandlers.ofString());
    }

    /** Whether {@code typed} is the passphrase this app unlocks with (what decrypts the stored seed). */
    static boolean unlocksSeed(String encryptedSeed, String typed) {
        if (encryptedSeed == null || encryptedSeed.isEmpty() || typed == null || typed.isEmpty()) {
            return false;
        }
        return HelperWallet.checkPassphrase(encryptedSeed, typed);
    }

    /**
     * Asks for the passphrase the app unlocks with before showing seed words: an unlocked app left
     * open should not show them to whoever sits down at it.
     */
    private void askPassphraseThen(String title, Runnable onUnlocked) {
        updateGui(() -> {
            VBox box = dialogRoot(title, "Enter the passphrase you unlock Outpost with to show the seed words.");
            javafx.scene.control.PasswordField field = new javafx.scene.control.PasswordField();
            field.setPromptText("Passphrase");
            Label problem = new Label();
            problem.setStyle("-fx-text-fill: #e5534b;");
            javafx.scene.control.Button show = new javafx.scene.control.Button("Show seed");
            show.getStyleClass().add("dialog-button-danger");
            show.setDefaultButton(true);
            javafx.scene.control.Button cancel = new javafx.scene.control.Button("Cancel");
            cancel.getStyleClass().add("dialog-button-secondary");
            cancel.setCancelButton(true);
            javafx.scene.layout.HBox buttons = new javafx.scene.layout.HBox(10, cancel, show);
            buttons.setAlignment(javafx.geometry.Pos.CENTER_RIGHT);
            box.getChildren().addAll(field, problem, buttons);

            Stage stage = new Stage();
            stage.initModality(javafx.stage.Modality.APPLICATION_MODAL);
            stage.setTitle(title);
            Scene scene = new Scene(box, Color.web("#0e0e0e"));
            GUISwap.applyTheme(scene);
            stage.setScene(scene);
            stage.setResizable(false);
            cancel.setOnAction(e -> stage.close());
            stage.setOnHidden(e -> field.clear());
            show.setOnAction(e -> {
                String typed = field.getText();
                show.setDisable(true);
                problem.setText("Checking...");
                GUISwap.executorService.submit(() -> {
                    boolean ok = unlocksSeed(HelperProperties.mnemonicEncrypted, typed);
                    if (!ok) {
                        try {
                            Thread.sleep(1000); // slow down guessing
                        } catch (InterruptedException ignored) {
                            Thread.currentThread().interrupt();
                        }
                    }
                    updateGui(() -> {
                        show.setDisable(false);
                        if (ok) {
                            stage.close();
                            onUnlocked.run();
                        } else {
                            field.clear();
                            problem.setText("That is not the passphrase this app unlocks with.");
                        }
                    });
                });
            });
            stage.show();
            stage.toFront();
            field.requestFocus();
        });
    }

    // The Bitcoin wallet the app was connected with or created: 12 or 24 BIP39 words. A wallet made
    // in Outpost exists nowhere else, so these words (and its passphrase) are its only backup.
    @FXML
    private void onShowBtcSeedClick(ActionEvent actionEvent) {
        askPassphraseThen("Bitcoin Wallet Seed", this::showBtcSeed);
    }

    private void showBtcSeed() {
        java.util.List<String> words;
        try {
            words = GUISwap.appSwap.getKeyChain().getSeed().getMnemonicCode();
        } catch (Exception e) {
            words = null;
        }
        if (words == null || words.isEmpty()) {
            updateGui(() -> showDialog("Bitcoin Wallet Seed", dialogRoot("Bitcoin Wallet Seed", "The wallet's seed words are not available.")));
            return;
        }
        String mnemonic = String.join(" ", words);
        boolean bip39 = HelperProperties.hasPassphrase;
        updateGui(() -> {
            VBox box = dialogRoot("Bitcoin Wallet Seed", bip39
                    ? "The words of this app's Bitcoin wallet. To restore it, in Ashigaru or any BIP39 wallet, you need these words AND your passphrase: the words alone open a different, empty wallet."
                    : "The words of this app's Bitcoin wallet. Restore with them in any BIP39 wallet.");
            VBox warning = new VBox(4);
            warning.getStyleClass().add("dialog-warning");
            Label warningTitle = new Label("KEEP THIS OFFLINE");
            warningTitle.getStyleClass().add("dialog-warning-title");
            Label warningBody = new Label("Write these words down on paper. Anyone with them" + (bip39 ? " and your passphrase" : "") + " can spend this wallet's bitcoin.");
            warningBody.getStyleClass().add("dialog-warning-body");
            warningBody.setWrapText(true);
            warning.getChildren().addAll(warningTitle, warningBody);
            VBox card = new VBox(8);
            card.getStyleClass().add("app-card");
            Label name = new Label("SEED PHRASE");
            name.getStyleClass().add("dialog-section-title");
            GridPane grid = new GridPane();
            grid.setHgap(18);
            grid.setVgap(6);
            String[] list = mnemonic.split(" ");
            int columns = 3;
            int rows = (list.length + columns - 1) / columns;
            for (int i = 0; i < list.length; i++) {
                // Numbered, filled down the columns: 1-4 in the first column for 12 words
                Label word = new Label(String.format("%2d  %s", i + 1, list[i]));
                word.getStyleClass().add("import-mono");
                grid.add(word, i / rows, i % rows);
            }
            card.getChildren().addAll(name, grid);
            box.getChildren().addAll(warning, card);
            showDialog("Bitcoin Wallet Seed", box);
        });
    }

    private void showSeedPopup(String mnemonic, String errorMessage) {
        updateGui(() -> {
            if (mnemonic == null) {
                VBox box = dialogRoot("ASB Wallet Seed", errorMessage);
                showDialog("ASB XMR Wallet Seed", box);
                return;
            }
            VBox box = dialogRoot("ASB Wallet Seed",
                    "The ASB's Monero wallet \"asb-wallet\". Restore with these words in any Monero wallet to recover funds.");

            VBox warning = new VBox(4);
            warning.getStyleClass().add("dialog-warning");
            Label warningTitle = new Label("KEEP THIS OFFLINE");
            warningTitle.getStyleClass().add("dialog-warning-title");
            Label warningBody = new Label("Write these 25 words down. Anyone with them can spend the ASB's Monero.");
            warningBody.getStyleClass().add("dialog-warning-body");
            warningBody.setWrapText(true);
            warning.getChildren().addAll(warningTitle, warningBody);

            VBox card = new VBox(8);
            card.getStyleClass().add("app-card");
            Label name = new Label("SEED PHRASE");
            name.getStyleClass().add("dialog-section-title");
            GridPane words = new GridPane();
            words.setHgap(18);
            words.setVgap(6);
            String[] list = mnemonic.trim().split("\\s+");
            int rows = (list.length + 4) / 5;
            for (int i = 0; i < list.length; i++) {
                // Numbered, filled down the columns: 1-5 in the first column, 6-10 in the next ...
                Label word = new Label(String.format("%2d  %s", i + 1, list[i]));
                word.getStyleClass().add("import-mono");
                words.add(word, i / rows, i % rows);
            }
            card.getChildren().addAll(name, words);
            box.getChildren().addAll(warning, card);
            showDialog("ASB XMR Wallet Seed", box);
        });
    }
}
