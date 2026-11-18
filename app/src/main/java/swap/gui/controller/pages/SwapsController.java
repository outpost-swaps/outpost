package swap.gui.controller.pages;

import swap.gui.controller.popups.PopupActiveSwapWarning;
import com.samourai.wallet.hd.AddressType;
import com.samourai.whirlpool.client.wallet.beans.WhirlpoolAccount;
import de.jensd.fx.glyphs.fontawesome.FontAwesomeIconView;
import javafx.application.Platform;
import javafx.beans.property.SimpleStringProperty;
import javafx.beans.value.ChangeListener;
import javafx.collections.FXCollections;
import javafx.collections.ObservableList;
import javafx.fxml.FXML;
import javafx.geometry.Insets;
import javafx.geometry.Pos;
import javafx.scene.control.*;
import javafx.scene.image.Image;
import javafx.scene.image.ImageView;
import javafx.scene.layout.GridPane;
import javafx.scene.layout.HBox;
import javafx.scene.layout.StackPane;
import javafx.scene.layout.VBox;
import javafx.scene.paint.Color;
import javafx.scene.text.Font;
import javafx.scene.text.FontWeight;
import javafx.scene.text.Text;
import org.bitcoinj.core.Coin;
import org.bitcoinj.core.NetworkParameters;
import org.bitcoinj.crypto.MnemonicException;
import org.bitcoinj.params.MainNetParams;
import org.bitcoinj.params.TestNet3Params;
import swap.gui.GUISwap;
import swap.gui.controller.BaseController;
import swap.gui.controller.MainController;
import swap.gui.debug.SwapUiDemo;
import swap.gui.node.PercentTableColumn;
import swap.gui.node.PercentTableView;
import swap.helper.HelperAddress;
import swap.helper.HelperSwapEstimate;
import swap.helper.SelfAsbPolicy;
import swap.helper.HelperProperties;
import swap.helper.HelperWallet;
import swap.lib.AppSwap;
import swap.model.*;
import swap.model.request.ListSellersDirectRequest;
import swap.model.request.ListSellersRequest;
import swap.model.request.SwapRequest;
import swap.model.response.ListSellersResponse;
import swap.wallet.SwapsWallet;

import javax.annotation.Nullable;
import java.io.File;
import java.nio.file.Files;
import java.net.URL;
import java.util.*;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicLong;

public class SwapsController extends BaseController {
    /** After this long the counter says it is waiting for slow servers (it keeps listening). */
    private static final long SELLER_REFRESH_SLOW_NOTICE_SECONDS = 30L;
    /** The native seller scan's own hard deadline: every server that has not answered by now counts as done. */
    private static final long SELLER_REFRESH_HARD_DEADLINE_SECONDS = 90L;
    private static final ScheduledExecutorService SELLER_REFRESH_TIMEOUTS =
            Executors.newSingleThreadScheduledExecutor(runnable -> {
                Thread thread = new Thread(runnable, "seller-refresh-timeout");
                thread.setDaemon(true);
                return thread;
            });

    public static SwapsController instance = null;
    private final AtomicLong sellerRefreshGeneration = new AtomicLong();
    /**
     * One refresh at a time. Several things ask for a refresh (client start, the ASB finishing its start,
     * the empty-list retry, the button): starting a new one threw the nearly finished one away and the
     * counter went back to 0. A request that arrives meanwhile is remembered and run once, right after.
     */
    private final AtomicBoolean sellerRefreshRunning = new AtomicBoolean(false);
    private final AtomicBoolean sellerRefreshQueued = new AtomicBoolean(false);
    /** Refreshes in a row that found hardly any seller (Tor not ready yet): retried sooner than the 10-minute cycle. */
    private final AtomicLong emptySellerRefreshes = new AtomicLong();
    /** Refreshes in a row each listed seller has not answered (see SellerListMerge). */
    private final ConcurrentHashMap<String, Integer> sellerMisses = new ConcurrentHashMap<>();
    private static final long EMPTY_REFRESH_RETRY_SECONDS = 45L;
    private static final long EMPTY_REFRESH_MAX_RETRIES = 4L;
    /** A refresh that finds fewer sellers than this is treated as "Tor is still warming up" (a healthy one finds 15 or more). */
    private static final int SPARSE_SELLER_COUNT = 3;
    private ScheduledFuture<?> preparingHeartbeat;
    private ScheduledFuture<?> resumeHeartbeat;
    public final ObservableList<Seller> sellersObservableList = FXCollections.synchronizedObservableList(FXCollections.observableArrayList());
    @FXML
    public GridPane swapsPane;
    @FXML
    public Label swapTitleLabel;
    @FXML
    public Label moneroAddressLabel;
    @FXML
    public TextField moneroAddressTextField;
    @FXML
    public Label moneroSellerLabel;
    @FXML
    public TextField libp2pPeerTextField;
    @FXML
    public Button startButton;
    @FXML
    public Button clearButton;
    @FXML
    public Button cancelButton;
    @FXML
    public VBox swapCenterBox;
    @FXML
    public HBox swapActionsBox;
    @FXML
    public VBox swapContextBox;
    @FXML
    public Label swapContextSeller;
    @FXML
    public Label swapContextAmount;
    @FXML
    public Label swapContextPrice;
    @FXML
    public Label swapContextXmr;
    @FXML
    public ProgressBar swapProgressBar;
    @FXML
    public VBox swapReceivedBox;
    @FXML
    public Label swapReceivedAmount;
    @FXML
    public Label swapReceivedCaption;
    @FXML
    public ImageView swapImage;
    @FXML
    public StackPane swapImageBox;
    @FXML
    public VBox swapStatusBox;
    @FXML
    public VBox swapIdleHint;
    @FXML
    public Label swapStatusText;
    @FXML
    public TextField swapStatusDetail;
    @FXML
    public VBox swapTracker;
    @FXML
    public FontAwesomeIconView swapIcon;
    @FXML
    public PercentTableView<Seller> sellerTable;
    @FXML
    public Button sellerRefreshButton;
    @FXML
    public Label xmrAvgPriceText;
    @FXML
    public Label refreshSellerText;
    @FXML
    public Label totalXmrForSaleInBtcText;
    @FXML
    private VBox xmrPriceBox;
    @FXML
    private VBox btcPriceBox;
    @FXML
    private Label xmrPriceValue;
    @FXML
    private Label btcPriceValue;
    @FXML
    private TextField payBtcField;
    @FXML
    private TextField receiveXmrField;
    @FXML
    private Label estimateNote;
    @FXML
    private VBox estimateRow;
    @FXML
    private HBox priceBoxes;

    /** Set while one field writes the other, so the listeners do not fight. */
    private boolean amountsSyncing;
    /** Which field the user typed into last; the other one is derived from it. */
    private boolean btcIsDriver = true;

    /** Empties both amount boxes and starts over from the Bitcoin side (Clear, and after a Disconnect). */
    private void clearAmounts() {
        if (payBtcField == null || receiveXmrField == null) {
            return;
        }
        amountsSyncing = true;
        try {
            payBtcField.clear();
            receiveXmrField.clear();
        } finally {
            amountsSyncing = false;
        }
        btcIsDriver = true;
        updateEstimate();
    }

    /** Fills the amount the user did not type, from the selected seller's price and limits. */
    private void updateEstimate() {
        if (estimateNote == null || payBtcField == null || receiveXmrField == null) {
            return;
        }
        Seller seller = HelperSwapEstimate.sellerFor(sellerTable.getTableView().getSelectionModel().getSelectedItem(),
                libp2pPeerTextField.getText(), sellersObservableList);
        Double min = seller == null ? null : seller.minQuantity();
        Double max = seller == null ? null : seller.maxQuantity();
        Double price = seller == null ? null : seller.price();
        HelperSwapEstimate.Conversion conversion = btcIsDriver
                ? HelperSwapEstimate.fromBtc(payBtcField.getText(), min, max, price)
                : HelperSwapEstimate.fromXmr(receiveXmrField.getText(), min, max, price);

        amountsSyncing = true;
        try {
            if (btcIsDriver && conversion.xmr() != null) {
                receiveXmrField.setText(HelperSwapEstimate.formatXmr(conversion.xmr()));
            } else if (!btcIsDriver && conversion.btc() != null) {
                payBtcField.setText(HelperSwapEstimate.formatBtc(conversion.btc()));
            } else if (HelperSwapEstimate.TOO_SMALL.equals(conversion.note())) {
                (btcIsDriver ? receiveXmrField : payBtcField).clear();
            }
        } finally {
            amountsSyncing = false;
        }

        estimateNote.setText(noteText(conversion, price));
    }

    private static String noteText(HelperSwapEstimate.Conversion conversion, Double price) {
        String note = conversion.note();
        if (HelperSwapEstimate.NO_SELLER.equals(note)) {
            return "Pick a seller first";
        }
        if (HelperSwapEstimate.BELOW_MIN.equals(note)) {
            return "Below the seller's minimum";
        }
        if (HelperSwapEstimate.ABOVE_MAX.equals(note)) {
            return "Above the seller's maximum";
        }
        if (HelperSwapEstimate.TOO_SMALL.equals(note)) {
            return "Too small to cover the fees";
        }
        if (!conversion.ok() || price == null || price <= 0) {
            return "Pick a seller first";
        }
        return "After the lock and sweep fees";
    }

    private final ChangeListener<Seller> sellerClickListener = (observable, oldValue, newValue) -> {
        if (newValue != null) {
            updateGui(() -> {
                if (isOwnAsbBlocked(newValue.multiaddr())) {
                    MainController.getInstance().printSwapLogLn(LogType.WARN,
                            "Can't trade with your own ASB in a release build. Select a different seller.", false);
                    return;
                }
                libp2pPeerTextField.setText(newValue.multiaddr());
                xmrPriceBox.setVisible(true);
                xmrPriceBox.setManaged(true);
                btcPriceBox.setVisible(true);
                btcPriceBox.setManaged(true);
                String btcPriceString = String.format("%.6f", 1 / newValue.price());
                xmrPriceValue.setText(String.format("%.8f", newValue.price()) + " BTC");
                btcPriceValue.setText(btcPriceString + " XMR");
            });
        }
    };
    private Coin currentXmrPriceInBtc = Coin.ZERO;
    private Coin currentAvgSellerPriceInBtc = null;
    private String refundAddress;

    private static final String[] SWAP_STEP_LABELS = {"Send BTC", "BTC locked", "XMR locked", "BTC redeemed", "XMR redeemed"};
    private static final String TRACKER_TODO_ICON = "CIRCLE_ALT";
    private static final String TRACKER_CURRENT_ICON = "CIRCLE";
    private static final String TRACKER_DONE_ICON = "CHECK_CIRCLE";
    private static final String TRACKER_FAILED_ICON = "TIMES_CIRCLE";
    private static final String COLOR_STATUS_DEFAULT = "#e7eaf0";
    private static final String COLOR_TRACKER_TODO = "#4d5259";
    private static final String COLOR_TRACKER_TEXT_TODO = "#8a8f96";
    private static final String COLOR_TRACKER_TEXT_DONE = "#9aa0a6";
    private static final String COLOR_DONE = "#27be4d";
    private static final String COLOR_ACCENT = "#8f1a14";
    private static final String COLOR_ERROR = "#c0201a";
    private final List<FontAwesomeIconView> swapTrackerIcons = new ArrayList<>();
    private final List<Label> swapTrackerLabels = new ArrayList<>();
    private final List<Label> swapTrackerDetails = new ArrayList<>();
    private int currentSwapStep = 0;
    private String currentSwapStepDetail = null;
    private boolean swapTerminal = false;
    private boolean swapStateReported = false;
    private Long swapContextAmountSatoshis = null;
    private Double swapContextResumePrice = null;

    @Override
    public void updateGui(Runnable runnable) {
        if (Platform.isFxApplicationThread()) runnable.run();
        else Platform.runLater(runnable);
    }

    @Override
    public void initialize(URL url, ResourceBundle rb) {
        instance = this;
        swapTitleLabel.visibleProperty().bind(swapContextBox.visibleProperty());
        swapTitleLabel.managedProperty().bind(swapContextBox.managedProperty());
        setSellersTable();
        sellerTable.getTableView().setPlaceholder(placeholderLabel("No sellers yet — press refresh"));
        cancelButton.setDisable(true);
//        setSwapImage(new Image("images/samourai-logo-white.png"), "Become Ungovernable", false);
        setSwapImage(new Image("images/outpost-mark.png"), "Become Ungovernable", false);
        Tooltip.install(xmrAvgPriceText, new Tooltip("Average price of the listed sellers"));
        Tooltip.install(sellerRefreshButton, new Tooltip("Refresh sellers"));
        buildSwapTracker();
        super.initialize(url, rb);
        loadCachedSpotPrice();
        SwapUiDemo.initIfEnabled(this, swapsPane);
    }

    private static Label placeholderLabel(String text) {
        Label label = new Label(text);
        label.setTextFill(Color.web("#9aa0a6"));
        return label;
    }

    @Override
    protected ScreenType getScreenType() {
        return ScreenType.SWAPS;
    }

    @Override
    protected void setupListeners() {
        sellerTable.getTableView().getSelectionModel().selectedItemProperty().addListener(sellerClickListener);
        sellerTable.getTableView().getSelectionModel().selectedItemProperty().addListener((o, a, b) -> updateEstimate());
        libp2pPeerTextField.textProperty().addListener((o, a, b) -> updateEstimate());
        sellersObservableList.addListener((javafx.collections.ListChangeListener<Seller>) change -> updateGui(this::updateEstimate));
        payBtcField.textProperty().addListener((o, a, b) -> {
            if (amountsSyncing) {
                return;
            }
            btcIsDriver = true;
            updateEstimate();
        });
        receiveXmrField.textProperty().addListener((o, a, b) -> {
            if (amountsSyncing) {
                return;
            }
            btcIsDriver = false;
            updateEstimate();
        });
        javafx.scene.control.Tooltip tip = new javafx.scene.control.Tooltip(
                "Either box fills the other. After the Bitcoin lock fee and the Monero sweep fee. "
                        + "The final amount is set after your deposit.");
        estimateNote.setTooltip(tip);
        // the two price boxes line up under the XMR amount field above them
        if (receiveXmrField.getParent() instanceof javafx.scene.layout.Region xmrSide) {
            priceBoxes.prefWidthProperty().bind(xmrSide.widthProperty());
            priceBoxes.minWidthProperty().bind(xmrSide.widthProperty());
            priceBoxes.maxWidthProperty().bind(xmrSide.widthProperty());
        }
    }

    // 'Buy XMR' Button
    @Override
    public Button getStartButton() {
        return startButton;
    }

    @Override
    public Button getStopButton() {
        return cancelButton;
    }

    public void setXmrPrice(Coin priceInBtc) {
        this.currentXmrPriceInBtc = priceInBtc;

        MainController mainController = MainController.getInstance();
        if (mainController != null)
            mainController.setNavSpotPrice(formatBtcAmount8(priceInBtc));

        updateGui(this::renderAvgPriceLabel);

        LiquidityController liquidityController = LiquidityController.getInstance();
        if (liquidityController != null) liquidityController.setSpotPrice(priceInBtc);

        saveSpotPrice(priceInBtc);
    }

    public Coin getCurrentXmrPriceInBtc() {
        return currentXmrPriceInBtc;
    }

    private void updateAvgSellerPrice(Coin avgPrice) {
        currentAvgSellerPriceInBtc = avgPrice;
        renderAvgPriceLabel();
    }

    private static String formatBtcAmount(Coin value) {
        return String.format("%.6f", value.value / 100_000_000.0) + " BTC";
    }

    private static String formatBtcAmount8(Coin value) {
        return String.format("%.8f", value.value / 100_000_000.0) + " BTC";
    }

    private void renderAvgPriceLabel() {
        Coin avgPrice = currentAvgSellerPriceInBtc;
        if (avgPrice == null || avgPrice.value <= 0) {
            xmrAvgPriceText.setText("Avg: —");
            xmrAvgPriceText.setVisible(true);
            return;
        }

        String text = "Avg: " + formatBtcAmount(avgPrice);
        if (currentXmrPriceInBtc != Coin.ZERO && currentXmrPriceInBtc.value > 0) {
            double pct = (avgPrice.value * 100.0 / currentXmrPriceInBtc.value) - 100.0;
            text += String.format(" (%+.2f%%)", pct);
        }

        xmrAvgPriceText.setText(text);
        if (!xmrAvgPriceText.isVisible())
            xmrAvgPriceText.setVisible(true);
    }

    private static final File LAST_SPOT_FILE = new File(AppSwap.getSwapRootDir(), "gui-last-spot-btc.txt");

    private void loadCachedSpotPrice() {
        try {
            if (!LAST_SPOT_FILE.exists()) return;
            Coin cached = Coin.parseCoin(Files.readString(LAST_SPOT_FILE.toPath()).trim());
            if (cached.value > 0) setXmrPrice(cached);
        } catch (Exception ignored) {
        }
    }

    private void saveSpotPrice(Coin priceInBtc) {
        try {
            Files.writeString(LAST_SPOT_FILE.toPath(), priceInBtc.toPlainString());
        } catch (Exception ignored) {
        }
    }

    private void setSellersTable() {
        // Widths add up to 100. The two narrow columns need room for the title plus the sort arrow:
        // at 10% each the arrow covered "%Δ" and cut "Refund" to "Ref…".
        PercentTableColumn<Seller, String> sellerColumn = new PercentTableColumn<>("Seller");
        sellerColumn.setPercentWidth(30);
        sellerColumn.setCellValueFactory(param -> new SimpleStringProperty(param.getValue().multiaddr()));

        PercentTableColumn<Seller, String> minColumn = new PercentTableColumn<>("Min. BTC");
        minColumn.setPercentWidth(15);
        minColumn.setCellValueFactory(param -> new SimpleStringProperty(Coin.parseCoin(String.valueOf(param.getValue().minQuantity())).toPlainString()));

        PercentTableColumn<Seller, String> maxColumn = new PercentTableColumn<>("Max. BTC");
        maxColumn.setPercentWidth(17);
        maxColumn.setCellValueFactory(param -> new SimpleStringProperty(Coin.parseCoin(String.valueOf(param.getValue().maxQuantity())).toPlainString()));

        PercentTableColumn<Seller, String> priceColumn = new PercentTableColumn<>("XMR Price");
        priceColumn.setPercentWidth(15);
        priceColumn.setCellValueFactory(param -> new SimpleStringProperty(Coin.parseCoin(String.valueOf(param.getValue().price())).toPlainString()));

        PercentTableColumn<Seller, String> refundColumn = new PercentTableColumn<>("Refund");
        refundColumn.setPercentWidth(12);
        refundColumn.setCellValueFactory(param -> {
            String policy = param.getValue().refundPolicy();
            return new SimpleStringProperty(policy == null || policy.isEmpty() ? "-" : policy);
        });

        PercentTableColumn<Seller, String> spreadColumn = new PercentTableColumn<>("% Δ");
        spreadColumn.setPercentWidth(11);
        spreadColumn.setText("% Δ");
        spreadColumn.setCellValueFactory(param -> {
            Coin sellerPrice = Coin.parseCoin(param.getValue().price() + "");
            if (currentXmrPriceInBtc != Coin.ZERO) {
                long currentPriceSatoshis = currentXmrPriceInBtc.value;
                long sellerPriceSatoshis = sellerPrice.value;
                long difference = sellerPriceSatoshis - currentPriceSatoshis;
                long percentage = Math.round(((double) difference / (double) currentPriceSatoshis) * 100d);
                String finalString = "≈ " + percentage + "%";
                return new SimpleStringProperty(finalString);
            } else {
                return new SimpleStringProperty("-");
            }
        });

        // These columns show text ("≈ 39%", "98.5%", "0.0004"), which sorts alphabetically: "≈ 39%" landed
        // between "≈ 5%" and "≈ 3%". Sort by the number in the text instead; a "-" sorts lowest.
        for (PercentTableColumn<Seller, String> numeric : List.of(minColumn, maxColumn, priceColumn, spreadColumn, refundColumn)) {
            numeric.setComparator(NumericTextSort::compare);
        }

        sellerTable.getTableView().getColumns().addAll(Arrays.asList(sellerColumn, minColumn, maxColumn, priceColumn, spreadColumn, refundColumn));
        sellerTable.getTableView().setItems(sellersObservableList);
        // Columns must always fit the visible width. The percent helper sizes them to the full width, so once the
        // list is long enough for a vertical scrollbar the columns overflow and a horizontal scrollbar appears.
        sellerTable.getTableView().setColumnResizePolicy(javafx.scene.control.TableView.CONSTRAINED_RESIZE_POLICY);
    }

    @FXML
    public void onStartButtonClick() {
        String xmrAddress = moneroAddressTextField.getText();
        String libp2pAddress = libp2pPeerTextField.getText();
        this.refundAddress = GUISwap.appSwap.getWalletSupplier().getWallet(WhirlpoolAccount.SWAPS_REFUNDS, AddressType.SEGWIT_NATIVE).getNextAddress().getAddressString(AddressType.SEGWIT_NATIVE); // TODO: test
//                ClientWhirlpool.getInstance().getWhirlpoolService().getChangeAddress(WhirlpoolAccount.SWAPS_REFUNDS);
        UUID uuid = UUID.randomUUID();
        startSwap(xmrAddress, libp2pAddress, refundAddress, uuid.toString());
    }

    @FXML
    public void onStopButtonClick() {
        // A stale terminal flag must never clear the panel while a client is
        // active (e.g. right after a resume starts).
        if (swapTerminal && !GUISwap.isSwapRunning()) {
            clearSwapDisplay();
            return;
        }
        // Disconnecting stops everything that watches the timelocks; with Bitcoin locked that
        // is how a refundable swap gets punished (audit M-5). Ask first, like closing the window.
        boolean bitcoinMayBeLocked = DisconnectPolicy.needsWarning(GUISwap.isSwapRunning(), currentSwapStep, currentSwapStepDetail);
        if (bitcoinMayBeLocked
                && !PopupActiveSwapWarning.confirmDisconnect((javafx.stage.Stage) cancelButton.getScene().getWindow(), activeSwapExitSummary())) {
            return;
        }
        swap.model.UserDisconnect.note(bitcoinMayBeLocked);
        // "Still preparing swap..." must not keep printing after the user has left it
        stopPreparingHeartbeat();
        MainController.getInstance().printSwapLogLn(LogType.INFO, "Disconnecting... This could take a couple minutes.", false);
        updateGui(() -> cancelButton.setDisable(true));
        endSwapThread();
    }


    @FXML
    public void onRefreshSellersButtonClick() {
        AppSwap appSwap = GUISwap.appSwap;
        if (appSwap == null) return;
        if (!sellerRefreshRunning.compareAndSet(false, true)) {
            sellerRefreshQueued.set(true);
            System.out.println("[SELLERS] refresh requested while one is running; it will run once after (from "
                    + Thread.currentThread().getName() + ")");
            return;
        }
        System.out.println("[SELLERS] refresh started (from " + Thread.currentThread().getName() + ")");
        updateGui(() -> sellerTable.getTableView().getSelectionModel().clearSelection());
        AtomicLong serversChecked = new AtomicLong();
        AtomicBoolean oursSeenInList = new AtomicBoolean(false);
        ConcurrentHashMap<String, Seller> sellerConcurrentHashMap = new ConcurrentHashMap<>();
        List<Multiaddr> rendezvousPeers = appSwap.getRendezvousPeers();
        // +1 = the direct known-makers query, which returns quotes even when every rendezvous point is down
        int totalServers = rendezvousPeers.size() + 1;
        long refreshGeneration = sellerRefreshGeneration.incrementAndGet();
        long startedAtMillis = System.currentTimeMillis();
        // The list on screen stays until this refresh is finished: a refresh starts with an empty
        // result, and replacing the table with the first few answers made the sellers vanish and return.
        ConcurrentHashMap<String, Seller> carriedOver = new ConcurrentHashMap<>();
        // The list this refresh settles on once every source has answered (or the deadline passed); late answers join it.
        java.util.concurrent.atomic.AtomicReference<java.util.Map<String, Seller>> settled = new java.util.concurrent.atomic.AtomicReference<>();
        updateGui(() -> {
            sellersObservableList.forEach(seller -> carriedOver.put(seller.multiaddr(), seller));
            this.refreshSellerText.setText("Checked 0/" + totalServers);
        });
        rendezvousPeers.forEach(rendezvousPeer -> {
            Thread listSellersThread = new Thread(() -> {
                ListSellersResponse sellersResponse = new ListSellersResponse(java.util.List.of());
                try {
                    if (HelperAddress.isLibp2pPeerValid(rendezvousPeer.toString()) && GUISwap.appSwap != null) {
                        ListSellersRequest listSellersRequest = new ListSellersRequest(GUISwap.appSwap.getSeedAsBase64(), rendezvousPeer.toString(), GUISwap.appSwap.getProxyPort(), GUISwap.appSwap.getParams());
                        ListSellersResponse response = GUISwap.appSwap.listSellers(listSellersRequest);
                        if (response != null) sellersResponse = response;
                    }
                } catch (Throwable error) {
                    System.err.println("Seller list refresh failed for " + rendezvousPeer + ": " + error);
                }
                // Every rendezvous must account for its server slot, even when
                // the address is invalid, the native call throws, or it returns
                // nothing. Otherwise the progress counter can never reach N/N.
                handleFetchedSellers(sellersResponse, serversChecked, oursSeenInList, sellerConcurrentHashMap, totalServers, refreshGeneration, carriedOver, settled, startedAtMillis);
            }, "seller-list-" + rendezvousPeer);
            listSellersThread.start();
        });

        Thread knownMakersThread = new Thread(() -> {
            ListSellersResponse sellersResponse = new ListSellersResponse(java.util.List.of());
            try {
                // Known-makers entries are mainnet addresses (sourced from the
                // upstream liquidity crawler); on testnet they would surface
                // mainnet makers to testnet takers — network mismatch. Skip there.
                if (GUISwap.appSwap != null && !appSwap.getParams().equals(TestNet3Params.get())) {
                    ListSellersDirectRequest directRequest = new ListSellersDirectRequest(GUISwap.appSwap.getSeedAsBase64(), GUISwap.appSwap.getKnownMakers(), GUISwap.appSwap.getProxyPort(), GUISwap.appSwap.getParams());
                    ListSellersResponse response = GUISwap.appSwap.listSellersDirect(directRequest);
                    if (response != null) sellersResponse = response;
                }
            } catch (Throwable error) {
                System.err.println("Direct seller list refresh failed: " + error);
            }
            // Testnet has no known-makers dial, but this still completes the
            // progress slot so the UI reaches N/N.
            handleFetchedSellers(sellersResponse, serversChecked, oursSeenInList, sellerConcurrentHashMap, totalServers, refreshGeneration, carriedOver, settled, startedAtMillis);
        }, "seller-list-known-makers");
        knownMakersThread.start();

        // After 30 s: say what is happening (slow servers are still being waited for, and their sellers
        // still join the table). The count is not forced to N/N here: that cleared the counter while
        // answers were still coming in, which looked like the refresh had finished with a short list.
        SELLER_REFRESH_TIMEOUTS.schedule(() -> {
            if (sellerRefreshGeneration.get() != refreshGeneration || serversChecked.get() >= totalServers) return;
            updateGui(() -> {
                if (sellerRefreshGeneration.get() != refreshGeneration) return;
                refreshSellerText.setText("Waiting " + serversChecked.get() + "/" + totalServers
                        + " · " + sellerConcurrentHashMap.size() + " sellers");
            });
        }, SELLER_REFRESH_SLOW_NOTICE_SECONDS, TimeUnit.SECONDS);

        // The native seller scan has a 90s hard deadline. A hung native call must not leave the
        // counter permanently below N/N.
        SELLER_REFRESH_TIMEOUTS.schedule(() -> {
            if (sellerRefreshGeneration.get() != refreshGeneration) return;
            long missing = totalServers - serversChecked.get();
            for (long i = 0; i < missing; i++) {
                handleFetchedSellers(new ListSellersResponse(java.util.List.of()),
                        serversChecked, oursSeenInList, sellerConcurrentHashMap, totalServers, refreshGeneration, carriedOver, settled, startedAtMillis);
            }
        }, SELLER_REFRESH_HARD_DEADLINE_SECONDS, TimeUnit.SECONDS);
    }

    private void handleFetchedSellers(ListSellersResponse sellersResponse,
                                      AtomicLong serversChecked,
                                      AtomicBoolean oursSeenInList,
                                      ConcurrentHashMap<String, Seller> sellerConcurrentHashMap,
                                      int totalServers,
                                      long refreshGeneration,
                                      ConcurrentHashMap<String, Seller> carriedOver,
                                      java.util.concurrent.atomic.AtomicReference<java.util.Map<String, Seller>> settled,
                                      long startedAtMillis) {
        int fetched = sellersResponse.sellers().size();
        if (fetched > 0) {
            sellersResponse.sellers()
                    .forEach(seller -> {
                        boolean isAsbRunning = GUISwap.isAsbRunning();
                        String ownPeerId = isAsbRunning ? GUISwap.appAsb.getPeerId() : "";
                        String ownAddress = isAsbRunning ? GUISwap.appAsb.getExternalAddress() : "";
                        boolean sellerIsUs = SelfAsbPolicy.isOwnAsb(seller.multiaddr(), ownPeerId, ownAddress);
                        if (sellerIsUs) {
                            oursSeenInList.set(true);

                            // update Liquidity page to display actual min/max & xmr price
                            updateGui(() -> {
                                LiquidityController.getInstance().minBtc.setText(Coin.parseCoin(String.valueOf(seller.minQuantity())).toPlainString());
                                LiquidityController.getInstance().maxBtc.setText(Coin.parseCoin(String.valueOf(seller.maxQuantity())).toPlainString());
                                LiquidityController.getInstance().setOurRate(seller.getPrice());
                            });
                            if (SelfAsbPolicy.shouldReject(
                                    seller.multiaddr(), isAsbRunning, ownPeerId, ownAddress,
                                    SelfAsbPolicy.isAllowedByEnvironment())) {
                                return;
                            }
                        }

                        sellerConcurrentHashMap.put(seller.multiaddr(), seller);
                    });
        }

        serversChecked.addAndGet(1);
        long checked = serversChecked.get();

        updateGui(() -> {
            // A newer refresh has started (the scheduled one and a manual click can overlap): this one's
            // answers must not replace the table or reset the counter, which made it flicker 9/10 -> 0/10.
            if (sellerRefreshGeneration.get() != refreshGeneration) return;
            try {
                boolean complete = checked >= totalServers;
                if (complete) {
                    // Finished. A seller that did not answer stays for a couple of refreshes, and a poor refresh
                    // (under half of what was known) removes nobody: a slow Tor made the list and its "Max" total
                    // shrink and recover. Answers that arrive after this point still join the list.
                    if (checked == totalServers) {
                        settled.set(SellerListMerge.afterRefresh(carriedOver, sellerConcurrentHashMap, sellerMisses));
                    }
                    java.util.LinkedHashMap<String, Seller> shown = new java.util.LinkedHashMap<>(
                            settled.get() != null ? settled.get() : carriedOver);
                    shown.putAll(sellerConcurrentHashMap);
                    sellersObservableList.setAll(shown.values());
                } else if (fetched > 0) {
                    // Still going: what was on screen, plus what has been found so far. Nothing disappears yet.
                    java.util.LinkedHashMap<String, Seller> shown = new java.util.LinkedHashMap<>(carriedOver);
                    shown.putAll(sellerConcurrentHashMap);
                    sellersObservableList.setAll(shown.values());
                }

                // Computed here, on the screen thread, from everything fetched so far: values worked out
                // on the refresh threads could arrive out of order and a stale "Max: —" overwrote the real one.
                swap.helper.HelperSellerStats.Stats stats = swap.helper.HelperSellerStats.of(new java.util.ArrayList<>(sellersObservableList));
                Coin total = stats.totalMax();
                totalXmrForSaleInBtcText.setText(total.value > 0 ? "Max: " + formatBtcAmount(total) : "Max: —");
                updateAvgSellerPrice(stats.averagePrice());

                if (complete) {
                    if (checked == totalServers) {
                        // This refresh is over: let the next one start, and run the one that asked meanwhile.
                        sellerRefreshRunning.set(false);
                        if (sellerRefreshQueued.getAndSet(false)) {
                            SELLER_REFRESH_TIMEOUTS.schedule(this::onRefreshSellersButtonClick, 1, TimeUnit.SECONDS);
                        }
                    }
                    refreshSellerText.setText("");
                    MainController.getInstance().setAsbListed(oursSeenInList.get());
                    if (checked == totalServers) {
                        System.out.println("[SELLERS] refresh done: " + checked + "/" + totalServers + " servers, "
                                + sellerConcurrentHashMap.size() + " sellers, "
                                + (System.currentTimeMillis() - startedAtMillis) / 1000 + " s");
                        // Right after start (or after hours closed) Tor is often not ready: most sources fail in
                        // seconds and the list stays nearly empty until the next 10-minute refresh. Try again soon.
                        if (sellerConcurrentHashMap.size() < SPARSE_SELLER_COUNT && emptySellerRefreshes.incrementAndGet() <= EMPTY_REFRESH_MAX_RETRIES) {
                            System.out.println("[SELLERS] only " + sellerConcurrentHashMap.size() + " seller(s) found; trying again in " + EMPTY_REFRESH_RETRY_SECONDS + " s");
                            refreshSellerText.setText("Few sellers yet · trying again");
                            SELLER_REFRESH_TIMEOUTS.schedule(() -> {
                                if (sellerRefreshGeneration.get() == refreshGeneration && !GUISwap.isSwapRunning()) {
                                    onRefreshSellersButtonClick();
                                }
                            }, EMPTY_REFRESH_RETRY_SECONDS, TimeUnit.SECONDS);
                        } else if (sellerConcurrentHashMap.size() >= SPARSE_SELLER_COUNT) {
                            emptySellerRefreshes.set(0);
                        }
                    }
                } else {
                    refreshSellerText.setText("Checked " + checked + "/" + totalServers
                            + " · " + sellerConcurrentHashMap.size() + " sellers");
                }
            } catch (Exception ignored) {
            }
        });
    }

    private void startPreparingHeartbeat() {
        stopPreparingHeartbeat();
        java.util.concurrent.atomic.AtomicLong ticks = new java.util.concurrent.atomic.AtomicLong();
        preparingHeartbeat = GUISwap.scheduledExecutorService.scheduleAtFixedRate(() -> {
            long seconds = swap.model.HeartbeatText.atTick(ticks.incrementAndGet(), 30);
            MainController controller = MainController.getInstance();
            if (controller != null) {
                controller.printSwapLogLn(LogType.INFO, swap.model.HeartbeatText.preparingSwap(seconds), false);
            }
        }, 30, 30, TimeUnit.SECONDS);
    }

    public void stopPreparingHeartbeat() {
        if (preparingHeartbeat != null) {
            preparingHeartbeat.cancel(false);
            preparingHeartbeat = null;
        }
    }

    public void startResumeHeartbeat() {
        stopResumeHeartbeat();
        java.util.concurrent.atomic.AtomicLong ticks = new java.util.concurrent.atomic.AtomicLong();
        resumeHeartbeat = GUISwap.scheduledExecutorService.scheduleAtFixedRate(() -> {
            long seconds = swap.model.HeartbeatText.atTick(ticks.incrementAndGet(), 30);
            MainController controller = MainController.getInstance();
            if (controller != null) {
                // Only runs during actual resume work; the swap-state callbacks
                // (onBtcLocked / onBtcLockConfirm / onXmrLockProofReceived) stop it.
                controller.printSwapLogLn(LogType.INFO, swap.model.HeartbeatText.resumingSwap(seconds), false);
            }
        }, 30, 30, TimeUnit.SECONDS);
    }

    public void stopResumeHeartbeat() {
        if (resumeHeartbeat != null) {
            resumeHeartbeat.cancel(false);
            resumeHeartbeat = null;
        }
    }

    private boolean isOwnAsbBlocked(String multiaddr) {
        boolean asbRunning = GUISwap.isAsbRunning();
        String peerId = asbRunning ? GUISwap.appAsb.getPeerId() : "";
        String ownAddress = asbRunning ? GUISwap.appAsb.getExternalAddress() : "";
        return SelfAsbPolicy.shouldReject(
                multiaddr, asbRunning, peerId, ownAddress, SelfAsbPolicy.isAllowedByEnvironment());
    }

    public void startSwap(String xmrAddress, String libp2pPeer, String refundAddress, String uuid) {
        MainController mainController = MainController.getInstance();
        boolean asbRunning = GUISwap.isAsbRunning();
        String peerId = asbRunning ? GUISwap.appAsb.getPeerId() : "";
        String ownAddress = asbRunning ? GUISwap.appAsb.getExternalAddress() : "";
        boolean selfAsbAllowed = SelfAsbPolicy.isAllowedByEnvironment();
        String sellerMultiaddr = SelfAsbPolicy.resolveSelfAsbMultiaddr(
                libp2pPeer, peerId, ownAddress, selfAsbAllowed);
        System.out.println("[SELF-ASB] allowed=" + selfAsbAllowed + " asbRunning=" + asbRunning
                + " peerId=" + peerId + " own=" + ownAddress
                + " seller=" + libp2pPeer + " resolved=" + sellerMultiaddr);
        if (!HelperAddress.isXmrAddrValid(xmrAddress)) {
            mainController.printSwapLogLn(LogType.INFO, "Please enter a valid Monero address", false);
        } else if (!HelperAddress.isLibp2pPeerValid(sellerMultiaddr)) {
            mainController.printSwapLogLn(LogType.INFO, "Please select a seller from the sellers list to the right", false);
        } else if (isOwnAsbBlocked(libp2pPeer)) {
            mainController.printSwapLogLn(LogType.WARN,
                    "Can't trade with your own ASB in a release build. Select a different seller.", false);
        } else {
            if (!mainController.getHistoryController().checkIncompleteSwaps()) {
                if (GUISwap.isSwapRunning()) {
                    mainController.printSwapLogLn(LogType.INFO, "Could not start swap: Another swap is already running.", false);
                    return;
                }
                String message = "Preparing to exchange Bitcoin for Monero... Please wait...";
                startPreparingHeartbeat();
                updateGui(() -> {
                    beginSwapAction();
                    startButton.setDisable(true);
                    setProgress(-1.0f);
                    prepareSwapDisplay("Preparing swap...", "preparing");
                });
                boolean started = maybeStartSwapThread(() -> {
                    mainController.printSwapLogLn(LogType.INFO, message, false);
                    GUISwap.appSwap.getProxy().ifPresentOrElse(proxy -> {
                        if (HelperProperties.dojoUrl == null || HelperProperties.dojoUrl.isBlank()
                                || HelperProperties.dojoApiKey == null || HelperProperties.dojoApiKey.isBlank()) {
                            failSwapStart("Dojo fee credentials are unavailable; refusing to start a swap without Dojo.");
                            return;
                        }
                        SwapsWallet swapsWallet = SwapsWallet.getInstance();
                        int swapsReceiveIndex = swapsWallet.getNextReceiveIndex(WhirlpoolAccount.SWAPS_DEPOSIT, AddressType.SEGWIT_NATIVE);
                        int swapsChangeIndex = swapsWallet.getNextChangeIndex(WhirlpoolAccount.SWAPS_DEPOSIT, AddressType.SEGWIT_NATIVE);
                        SwapRequest swapRequest = new SwapRequest(uuid, GUISwap.appSwap.getSeedAsBase64(), xmrAddress.trim(), GUISwap.appSwap.getElectrumServer(), sellerMultiaddr.trim(), proxy, GUISwap.appSwap.getParams(), GUISwap.appSwap.getProxyPort(), refundAddress, WhirlpoolAccount.SWAPS_DEPOSIT.getAccountIndex(), swapsReceiveIndex, swapsChangeIndex, HelperProperties.dojoUrl, HelperProperties.dojoApiKey);
                        try {
                            GUISwap.appSwap.buyXmr(swapRequest);
                            // The client is registered and its worker is now
                            // requesting a quote. Let the user abort a hanging
                            // quote instead of waiting for the first listener
                            // callback (which only arrives after the quote).
                            updateGui(() -> {
                                if (!swapTerminal) cancelButton.setDisable(false);
                            });
                        } catch (RuntimeException e) {
                            mainController.printSwapLogLn(LogType.ERROR, "Failed to start swap: " + e.getMessage(), false);
                            endSwapThread();
                            swapKillReset(false);
                        }
                    }, () -> failSwapStart("Tor is not ready yet, so the swap was not started. Wait for the Tor indicator to turn ready and try again."));
                });


                if (!started) {
                    stopPreparingHeartbeat();
                    updateGui(this::resetSwapDisplay);
                    mainController.printSwapLogLn(LogType.INFO, "Could not start swap: Another swap is already running.", false);
                }
            }
        }

    }

    /**
     * A swap that could not even start (Tor not ready, no Dojo credentials). The
     * start button used to stay disabled on "Preparing swap..." with nothing but
     * a heartbeat in the log, because these branches just returned.
     */
    public void failSwapStart(String message) {
        MainController.getInstance().printSwapLogLn(LogType.ERROR, message, false);
        stopPreparingHeartbeat();
        stopResumeHeartbeat();
        endSwapThread();
        updateGui(() -> {
            resetSwapDisplay();
            startButton.setDisable(false);
        });
    }

    public void swapKillReset(boolean clearFields) {
        // TODO: improve ui displaying of errors/cancels/successes, maybe don't clear all
        updateGui(() -> {
            // Hide the bar but keep its layout slot so terminal panels do not
            // jump upward. Direct field writes avoid a queued setProgress(0)
            // landing after a caller's setProgress(-1) and hiding it again.
            swapProgressBar.setProgress(0.0f);
            swapProgressBar.setManaged(true);
            swapProgressBar.setVisible(false);
            startButton.setDisable(false);
            if (swapTracker.isVisible()) {
                swapTerminal = true;
                cancelButton.setText("Clear");
                cancelButton.setVisible(true);
                cancelButton.setManaged(true);
            } else {
                swapProgressBar.setManaged(false);
                swapProgressBar.setVisible(false);
            }
            xmrPriceBox.setVisible(false);
            xmrPriceBox.setManaged(false);
            btcPriceBox.setVisible(false);
            btcPriceBox.setManaged(false);
        });

        if (clearFields) {
            updateGui(() -> {
                libp2pPeerTextField.setText(null);
                moneroAddressTextField.setText(null);
            });
        }

        HelperWallet.deleteAllFilesInFolder(new File(AppSwap.getSwapRootDir(), "tmp"), false);
    }

    public void onClearButtonClick() {
        if (!GUISwap.isSwapRunning()) {
            clearSwapDisplay();
        } else {
            MainController.getInstance().printSwapLogLn(LogType.HIGHLIGHT, "Can't clear fields while swap is running.", false);
        }
    }

    private void clearSwapDisplay() {
        updateGui(() -> {
            xmrPriceBox.setVisible(false);
            xmrPriceBox.setManaged(false);
            btcPriceBox.setVisible(false);
            btcPriceBox.setManaged(false);
            libp2pPeerTextField.setText(null);
            moneroAddressTextField.setText(null);
            resetSwapDisplay();
            sellerTable.getTableView().getSelectionModel().clearSelection();
        });
    }

    public void setProgress(float progress) {
        updateGui(() -> {
            if (swapTerminal && progress != 0.0f) return;
            swapProgressBar.setProgress(progress);
            swapProgressBar.setManaged(progress != 0.0f);
            swapProgressBar.setVisible(progress != 0.0f);
        });
    }

    /** Hide the progress bar at a terminal state but keep its layout slot. */
    public void reserveSwapProgressSpace() {
        updateGui(() -> {
            swapProgressBar.setProgress(0.0f);
            swapProgressBar.setManaged(true);
            swapProgressBar.setVisible(false);
        });
    }

    /** The status card's normal height, and its height with the received amount inside. */
    static final double STATUS_CARD_HEIGHT = 54.0;
    /** 54 + the progress bar's 46 px slot, which collapses while the amount shows. */
    static final double STATUS_CARD_WITH_AMOUNT_HEIGHT = 100.0;

    /**
     * Swap Success: the exact XMR received goes inside the status card, between "Swap Success"
     * and the swap ID (matrix N-15). The bar's slot collapses and the card grows by the same
     * 46 px, so the check and the step list do not move. Only called with amounts the engine
     * reported; without them the card stays as it is.
     */
    public void showXmrReceived(long receivedPiconero, long feePiconero) {
        updateGui(() -> {
            swapReceivedAmount.setText(SwapReceived.amountLine(receivedPiconero));
            swapReceivedCaption.setText(SwapReceived.caption(feePiconero));
            swapProgressBar.setManaged(false);
            swapProgressBar.setVisible(false);
            setStatusCardHeight(STATUS_CARD_WITH_AMOUNT_HEIGHT);
            swapReceivedBox.setManaged(true);
            swapReceivedBox.setVisible(true);
        });
    }

    private void hideXmrReceived() {
        swapReceivedBox.setManaged(false);
        swapReceivedBox.setVisible(false);
        swapReceivedAmount.setText("");
        swapReceivedCaption.setText("");
        setStatusCardHeight(STATUS_CARD_HEIGHT);
    }

    private void setStatusCardHeight(double height) {
        swapStatusBox.setMinHeight(height);
        swapStatusBox.setPrefHeight(height);
    }

    public void setSwapImage(Image image, String tooltip, boolean qrCode) {
        updateGui(() -> applySwapImage(image, tooltip, qrCode));
    }

    private void applySwapImage(Image image, String tooltip, boolean qrCode) {
        swapIcon.setVisible(false);
        swapIcon.setManaged(false);
        swapImage.setVisible(true);
        swapImage.setManaged(true);

        if (qrCode) {
            swapImage.setFitHeight(88);
            swapImage.setFitWidth(88);
            swapImageBox.setMinHeight(96);
            swapImageBox.setPrefHeight(96);
        } else {
            swapImage.setFitHeight(84);
            swapImage.setFitWidth(84);
            swapImageBox.setMinHeight(96);
            swapImageBox.setPrefHeight(96);
        }

        swapImage.setImage(image);
        Tooltip.install(swapImage, new Tooltip(tooltip));
    }

    public void setLockIcon(SwapIconType swapIconType) {
        updateGui(() -> {
            swapImage.setVisible(false);
            swapImage.setManaged(false);
            swapIcon.setVisible(true);
            swapIcon.setManaged(true);
            swapIcon.setGlyphName(swapIconType.getGlyphName());
            swapIcon.setFill(swapIconType.getColor());
            swapIcon.setSize(String.valueOf(swapIconType.getSize()));
            swapImageBox.setMinHeight(96);
            swapImageBox.setPrefHeight(96);
        });
    }

    private void buildSwapTracker() {
        swapTracker.getChildren().clear();
        swapTrackerIcons.clear();
        swapTrackerLabels.clear();
        swapTrackerDetails.clear();
        for (String stepLabel : SWAP_STEP_LABELS) {
            FontAwesomeIconView icon = new FontAwesomeIconView();
            icon.setGlyphName(TRACKER_TODO_ICON);
            icon.setFill(Color.web(COLOR_TRACKER_TODO));
            icon.setSize("12");
            Label label = new Label(stepLabel);
            label.setMinWidth(102);
            label.setFont(Font.font("Arial", 13));
            label.setTextFill(Color.web(COLOR_TRACKER_TEXT_TODO));
            Label detail = new Label();
            detail.setFont(Font.font("Menlo", 13));
            detail.setTextFill(Color.web(COLOR_TRACKER_TEXT_DONE));
            HBox row = new HBox(8, icon, label, detail);
            row.setAlignment(Pos.CENTER_LEFT);
            row.setMaxWidth(300);
            swapTracker.getChildren().add(row);
            swapTrackerIcons.add(icon);
            swapTrackerLabels.add(label);
            swapTrackerDetails.add(detail);
        }
    }

    public void prepareSwapDisplay(String headline, String stepDetail) {
        updateGui(() -> {
            // A fast resume (refund already confirmed) can finish before this
            // preparation runs. Never overwrite an already terminal panel.
            if (swapTerminal) return;
            boolean stateReported = swapStateReported;
            // Keep the idle logo until the client reports a concrete protocol
            // state; otherwise a stale icon (e.g. the refund hourglass) can
            // survive into the "Resuming swap..." screen.
            if (!stateReported) applySwapImage(new Image("images/outpost-mark.png"), "Become Ungovernable", false);
            swapTerminal = false;
            applySwapInputVisible(false);
            if (!stateReported) applySwapContext(moneroAddressTextField.getText(), libp2pPeerTextField.getText());
            swapContextBox.setManaged(true);
            swapContextBox.setVisible(true);
            swapIdleHint.setManaged(false);
            swapIdleHint.setVisible(false);
            hideXmrReceived();
            swapProgressBar.setManaged(true);
            if (!stateReported) swapProgressBar.setVisible(true);
            swapCenterBox.setAlignment(Pos.TOP_CENTER);
            cancelButton.setText("Disconnect");
            // Buy XMR registers the client on a background thread. Keep the
            // button disabled only until that client exists, then enable it so
            // a stalled quote request can be disconnected.
            cancelButton.setDisable(!GUISwap.isSwapRunning());
            cancelButton.setVisible(true);
            cancelButton.setManaged(true);
            swapStatusBox.setManaged(true);
            swapStatusBox.setVisible(true);
            swapTracker.setManaged(true);
            swapTracker.setVisible(true);
            if (!stateReported) {
                applySwapStatus(headline, null);
                applySwapStatusDetail("", null);
                // Resuming does not establish that a deposit has been sent.
                applySwapStep("resuming".equals(stepDetail) ? 0 : 1, stepDetail);
            }
        });
    }

    public void resetSwapDisplay() {
        updateGui(() -> {
            clearAmounts();
            swapTerminal = false;
            applySwapInputVisible(true);
            swapContextBox.setManaged(false);
            swapContextBox.setVisible(false);
            swapIdleHint.setManaged(true);
            swapIdleHint.setVisible(true);
            swapCenterBox.setAlignment(Pos.CENTER);
            cancelButton.setText("Disconnect");
            cancelButton.setVisible(false);
            cancelButton.setManaged(false);
            swapStatusBox.setManaged(false);
            swapStatusBox.setVisible(false);
            swapTracker.setManaged(false);
            swapTracker.setVisible(false);
            swapStatusText.setText("");
            swapStatusDetail.setText("");
            currentSwapStep = 0;
            applySwapImage(new Image("images/outpost-mark.png"), "Become Ungovernable", false);
            swapProgressBar.setProgress(0.0f);
            swapProgressBar.setManaged(false);
            swapProgressBar.setVisible(false);
            hideXmrReceived();
        });
    }

    private void applySwapInputVisible(boolean visible) {
        moneroAddressLabel.setVisible(visible);
        moneroAddressLabel.setManaged(visible);
        moneroAddressTextField.setVisible(visible);
        moneroAddressTextField.setManaged(visible);
        moneroSellerLabel.setVisible(visible);
        moneroSellerLabel.setManaged(visible);
        libp2pPeerTextField.setVisible(visible);
        libp2pPeerTextField.setManaged(visible);
        estimateRow.setVisible(visible);
        estimateRow.setManaged(visible);
        swapActionsBox.setVisible(visible);
        swapActionsBox.setManaged(visible);
    }

    private void applySwapContext(String xmrAddress, String seller) {
        setContextLine(swapContextXmr, xmrAddress, "XMR destination address");
        setContextLine(swapContextSeller, seller, "Seller");
        swapContextAmountSatoshis = null;
        swapContextResumePrice = null;
        applySwapAmountLine("—");
        applySwapPriceLine();
    }

    public void applyResumedSwapContext(String seller, String xmrAddress, long btcSatoshis, long xmrPiconero) {
        updateGui(() -> {
            setContextLine(swapContextSeller, seller, "Seller");
            setContextLine(swapContextXmr, xmrAddress, "XMR destination address");
            swapContextAmountSatoshis = btcSatoshis;
            swapContextResumePrice = xmrPiconero > 0
                    ? (btcSatoshis / 1e8) / (xmrPiconero / 1e12)
                    : null;
            applySwapAmountValue();
            applySwapPriceLine();
        });
    }

    public void setSwapContextAmount(long btcSatoshis) {
        updateGui(() -> {
            swapContextAmountSatoshis = btcSatoshis;
            applySwapAmountValue();
            applySwapPriceLine();
        });
    }

    public void refreshSwapContext() {
        updateGui(() -> {
            if (!swapContextBox.isVisible()) return;
            setContextLine(swapContextXmr, moneroAddressTextField.getText(), "XMR destination address");
            setContextLine(swapContextSeller, libp2pPeerTextField.getText(), "Seller");
            applySwapAmountValue();
            applySwapPriceLine();
        });
    }

    private void applySwapAmountValue() {
        if (swapContextAmountSatoshis == null) {
            applySwapAmountLine("—");
            return;
        }
        String btc = Coin.valueOf(swapContextAmountSatoshis).toPlainString();
        Double price = effectivePrice();
        if (price == null || price <= 0) {
            applySwapAmountLine(btc + " BTC");
        } else {
            applySwapAmountLine(btc + " BTC → ≈" + String.format("%.4f", (swapContextAmountSatoshis / 1e8) / price) + " XMR");
        }
    }

    private void applySwapAmountLine(String text) {
        swapContextAmount.setText(text);
        Tooltip.uninstall(swapContextAmount, swapContextAmount.getTooltip());
        if (!"—".equals(text)) {
            Tooltip.install(swapContextAmount, new Tooltip("Amount: " + text));
        }
    }

    /// Live seller quote when one is selected, otherwise the price implied by
    /// the resumed swap's recorded BTC/XMR amounts.
    private Double effectivePrice() {
        Double price = selectedSellerPrice();
        if (price != null && price > 0) return price;
        return swapContextResumePrice;
    }

    private void applySwapPriceLine() {
        Double price = effectivePrice();
        String text = price == null || price <= 0 ? "—" : String.format("%.8f", price) + " BTC";
        swapContextPrice.setText(text);
        Tooltip.uninstall(swapContextPrice, swapContextPrice.getTooltip());
        if (!"—".equals(text)) {
            Tooltip.install(swapContextPrice, new Tooltip("Rate: 1 XMR = " + text));
        }
    }

    private Double selectedSellerPrice() {
        String selected = libp2pPeerTextField.getText();
        if (selected != null && !selected.isBlank()) {
            for (Seller seller : sellersObservableList) {
                if (selected.trim().equals(seller.multiaddr())) return seller.price();
            }
        }
        return swapContextResumePrice;
    }

    private void setContextLine(Label label, String value, String tooltipPrefix) {
        String text = value == null || value.isBlank() ? "—" : value.trim();
        label.setText(text);
        try {
            Tooltip.uninstall(label, label.getTooltip());
        } catch (Exception ignored) {
        }
        if (!"—".equals(text)) {
            Tooltip.install(label, new Tooltip(tooltipPrefix + ": " + text));
        }
    }

    public void setSwapStatus(String text) {
        setSwapStatus(text, null);
    }

    public void setSwapStatus(String text, String colorHex) {
        updateGui(() -> applySwapStatus(text, colorHex));
    }

    private void applySwapStatus(String text, String colorHex) {
        if (!swapStatusBox.isManaged()) {
            swapStatusBox.setManaged(true);
            swapStatusBox.setVisible(true);
        }
        swapStatusText.setText(text);
        swapStatusText.setTextFill(Color.web(colorHex == null ? COLOR_STATUS_DEFAULT : colorHex));
        swapStatusText.setFont(colorHex == null
                ? Font.font("Arial", 13)
                : Font.font("Arial", FontWeight.BOLD, 13));
    }

    public void setSwapStatusDetail(String text, @Nullable String tooltip) {
        updateGui(() -> applySwapStatusDetail(text, tooltip));
    }

    private void applySwapStatusDetail(String text, @Nullable String tooltip) {
        String full = text == null ? "" : text;
        // Only single-token txids get width-fitting/ellipsis. Error text keeps
        // its existing character cap and is never reformatted here.
        swapStatusDetail.setText(isTxid(full) ? fitStatusDetailToWidth(full) : full);
        // TextField otherwise scrolls to the end when the text exceeds its
        // width, which looked right-aligned. Keep the start visible; long
        // technical detail belongs in the tooltip/swap log.
        swapStatusDetail.positionCaret(0);
        try {
            if (tooltip == null) {
                Tooltip.uninstall(swapStatusDetail, swapStatusDetail.getTooltip());
            } else {
                Tooltip.install(swapStatusDetail, new Tooltip(tooltip));
            }
        } catch (Exception ignored) {
        }
    }

    private static boolean isTxid(String text) {
        return text != null && text.trim().matches("[0-9a-fA-F]{64}");
    }

    /**
     * Fit a long txid in the fixed-width status square. TextField does not
     * wrap; measure the actual font and append an ellipsis instead of silently
     * clipping the token. This is not used for error messages.
     */
    private String fitStatusDetailToWidth(String text) {
        if (text == null || text.isEmpty() || swapStatusDetail == null) {
            return text;
        }
        double width = swapStatusDetail.getWidth();
        if (width <= 0) {
            width = swapStatusDetail.getPrefWidth();
        }
        if (width <= 0) {
            width = 320;
        }
        double available = Math.max(24, width - 8);
        Text measure = new Text();
        measure.setFont(swapStatusDetail.getFont());
        measure.setText(text);
        if (measure.getLayoutBounds().getWidth() <= available) {
            return text;
        }
        String ellipsis = "...";
        int low = 0;
        int high = text.length();
        while (low < high) {
            int mid = (low + high + 1) / 2;
            measure.setText(text.substring(0, mid) + ellipsis);
            if (measure.getLayoutBounds().getWidth() <= available) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        return text.substring(0, low) + ellipsis;
    }

    /**
     * Human-readable snapshot of the in-flight swap for the quit confirmation.
     * Must be called on the JavaFX thread.
     */
    public String activeSwapExitSummary() {
        StringBuilder summary = new StringBuilder();
        if (currentSwapStep >= 1 && currentSwapStep <= SWAP_STEP_LABELS.length) {
            summary.append("Current step: ")
                    .append(SWAP_STEP_LABELS[currentSwapStep - 1])
                    .append(" (")
                    .append(currentSwapStep)
                    .append("/")
                    .append(SWAP_STEP_LABELS.length)
                    .append(").");
        }

        String status = swapStatusText != null ? swapStatusText.getText() : null;
        if (status != null && !status.isBlank()) {
            if (summary.length() > 0) summary.append('\n');
            summary.append("Status: ").append(status).append('.');
        }

        String swapId = swapStatusDetail != null ? swapStatusDetail.getText() : null;
        if (swapId != null && !swapId.isBlank() && !"—".equals(swapId)) {
            if (summary.length() > 0) summary.append('\n');
            summary.append("Swap ID: ").append(swapId);
        }

        return summary.length() == 0 ? "A swap is still in progress." : summary.toString();
    }

    public void setSwapStep(int step, String detail) {
        updateGui(() -> {
            swapStateReported = true;
            applySwapStep(step, detail);
        });
    }

    private void applySwapStep(int step, String detail) {
        if (!swapTracker.isManaged()) {
            swapTracker.setManaged(true);
            swapTracker.setVisible(true);
        }
        currentSwapStep = step;
        currentSwapStepDetail = detail;
        for (int i = 0; i < swapTrackerIcons.size(); i++) {
            int stepNumber = i + 1;
            FontAwesomeIconView icon = swapTrackerIcons.get(i);
            Label label = swapTrackerLabels.get(i);
            Label stepDetail = swapTrackerDetails.get(i);
            if (step > stepNumber) {
                icon.setGlyphName(TRACKER_DONE_ICON);
                icon.setFill(Color.web(COLOR_DONE));
                label.setFont(Font.font("Arial", 13));
                label.setTextFill(Color.web(COLOR_TRACKER_TEXT_DONE));
                // A completed step reports its outcome, not its live counter:
                // once XMR lock confirmations finish, "10/10 confs" -> "confirmed".
                String doneText = stepDetail.getText();
                if (doneText != null && doneText.matches("[0-9]+/10 confs")) {
                    stepDetail.setText("confirmed");
                } else if (doneText == null || doneText.isBlank()) {
                    // a resumed swap never showed the early steps: say how they ended
                    stepDetail.setText(SwapStepLabels.completedDetail(stepNumber));
                }
            } else if (step == stepNumber) {
                icon.setGlyphName(TRACKER_CURRENT_ICON);
                icon.setFill(Color.web(COLOR_ACCENT));
                label.setFont(Font.font("Arial", FontWeight.BOLD, 13));
                label.setTextFill(Color.web(COLOR_STATUS_DEFAULT));
                stepDetail.setText(detail == null ? "" : detail);
            } else {
                icon.setGlyphName(TRACKER_TODO_ICON);
                icon.setFill(Color.web(COLOR_TRACKER_TODO));
                label.setFont(Font.font("Arial", 13));
                label.setTextFill(Color.web(COLOR_TRACKER_TEXT_TODO));
                stepDetail.setText("");
            }
        }
    }

    /** Render a successful terminal state; never invent a confirmation count. */
    public void completeSwapSteps() {
        updateGui(() -> {
            swapStateReported = true;
            String[] details = SwapStepLabels.completion();
            for (int i = 0; i < details.length; i++) {
                applySwapStep(i + 1, details[i]);
            }
            applySwapStep(6, null);
        });
    }

    public void failCurrentSwapStep() {
        updateGui(() -> {
            int index = currentSwapStep - 1;
            if (index < 0 || index >= swapTrackerIcons.size()) return;
            swapTrackerIcons.get(index).setGlyphName(TRACKER_FAILED_ICON);
            swapTrackerIcons.get(index).setFill(Color.web(COLOR_ERROR));
            swapTrackerLabels.get(index).setFont(Font.font("Arial", FontWeight.BOLD, 13));
            swapTrackerLabels.get(index).setTextFill(Color.web(COLOR_ERROR));
        });
    }

    public void markSwapCanceledStep() {
        updateGui(() -> {
            swapStateReported = true;
            applySwapStep(Math.max(currentSwapStep, 2), "canceled");
        });
    }

    public void markSwapRefundBroadcastStep() {
        updateGui(() -> {
            swapStateReported = true;
            applySwapStep(Math.max(currentSwapStep, 2), "refunding");
        });
    }

    public void markSwapRefundedStep() {
        updateGui(() -> {
            swapStateReported = true;
            applySwapStep(Math.max(currentSwapStep, 2), "refunded");
        });
    }

    /** Call on the FX thread. Keeps a late resume preparation from overwriting a terminal panel. */
    public void markSwapTerminal() {
        swapTerminal = true;
    }


    /**
     * Call on the FX thread before launching a swap/resume client. Clears the
     * terminal flag and switches the button to Disconnect before the client
     * exists, so a stale Clear can never wipe the panel mid-start.
     */
    public void beginSwapAction() {
        swap.model.UserDisconnect.clear();
        swapTerminal = false;
        swapStateReported = false;
        currentSwapStep = 0;
        cancelButton.setText("Disconnect");
        cancelButton.setDisable(true);
        cancelButton.setVisible(true);
        cancelButton.setManaged(true);
    }

    public void onClientStarted() {
        GUISwap.scheduledExecutorService.scheduleAtFixedRate(this::onScheduledSellersRefresh, 0,
                SellerRefreshPolicy.SCHEDULED_REFRESH_MINUTES, TimeUnit.MINUTES);
    }

    /** Periodic refresh; skipped while a swap runs so it cannot load the swap's Tor connections. */
    private void onScheduledSellersRefresh() {
        if (!SellerRefreshPolicy.shouldRunScheduledRefresh(GUISwap.isSwapRunning())) {
            return;
        }
        onRefreshSellersButtonClick();
    }

    /**
     * The live quote fetched at swap start is what governs the swap
     * (determine_btc_to_swap clamps to its max), but the sellers table shows a
     * snapshot from the last refresh (up to 10 minutes old) — price drift makes
     * the displayed max drift from the quoted max. Reconcile the selected
     * seller's row to the authoritative live quote so the display matches the
     * amount actually locked.
     */
    public void reconcileSelectedSellerWithLiveQuote(long liveMaxSatoshis) {
        String selectedMultiaddr = libp2pPeerTextField.getText();
        if (selectedMultiaddr == null || selectedMultiaddr.isBlank()) return;
        updateGui(() -> {
            for (int i = 0; i < sellersObservableList.size(); i++) {
                Seller seller = sellersObservableList.get(i);
                if (!selectedMultiaddr.trim().equals(seller.multiaddr())) continue;

                Coin liveMax = Coin.valueOf(liveMaxSatoshis);
                Coin displayedMax;
                try {
                    displayedMax = Coin.parseCoin(String.valueOf(seller.maxQuantity()));
                } catch (Exception e) {
                    return;
                }
                if (displayedMax.value != liveMax.value) {
                    MainController.getInstance().printSwapLogLn(LogType.INFO,
                            "Seller's advertised max. moved since the sellers list was refreshed: "
                                    + displayedMax.toPlainString() + " BTC -> " + liveMax.toPlainString()
                                    + " BTC (the live quote governs the swap)", false);
                    boolean wasSelected = i == sellerTable.getTableView().getSelectionModel().getSelectedIndex();
                    double liveMaxBtc = Double.parseDouble(liveMax.toPlainString());
                    sellersObservableList.set(i, new Seller(seller.multiaddr(), seller.minQuantity(), liveMaxBtc, seller.price(), seller.refundPolicy()));
                    if (wasSelected)
                        sellerTable.getTableView().getSelectionModel().select(i);
                }
                break;
            }
        });
    }

    public String getMoneroAddressText() {
        return moneroAddressTextField.getText();
    }

    public String getLibp2pPeerText() {
        return libp2pPeerTextField.getText();
    }

    public String getRefundAddress() {
        return this.refundAddress;
    }
}
