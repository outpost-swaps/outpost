package swap.gui.controller.pages;

import de.jensd.fx.glyphs.fontawesome.FontAwesomeIconView;
import javafx.animation.PauseTransition;
import javafx.beans.binding.Bindings;
import javafx.beans.property.SimpleStringProperty;
import javafx.collections.FXCollections;
import javafx.collections.ObservableList;
import javafx.fxml.FXML;
import javafx.scene.chart.PieChart;
import javafx.scene.control.*;
import javafx.scene.image.Image;
import javafx.scene.input.Clipboard;
import javafx.scene.input.ClipboardContent;
import javafx.scene.input.MouseButton;
import javafx.scene.input.MouseEvent;
import javafx.scene.layout.GridPane;
import javafx.scene.layout.HBox;
import javafx.scene.layout.StackPane;
import javafx.scene.paint.Color;
import javafx.scene.shape.Circle;
import javafx.util.Duration;
import org.json.JSONObject;
import swap.gui.GUISwap;
import swap.gui.controller.BaseController;
import swap.gui.controller.MainController;
import swap.helper.HelperProperties;
import swap.helper.HelperRawJsonDb;
import swap.helper.HelperSwapsDb;
import swap.model.LogType;
import swap.model.SwapData;
import swap.model.request.GetHistoryRequest;
import swap.model.request.ResumeRequest;
import swap.model.response.GetHistoryResponse;
import swap.wallet.SwapsWallet;
import com.samourai.wallet.hd.AddressType;
import com.samourai.whirlpool.client.wallet.beans.WhirlpoolAccount;

import java.net.URL;
import java.util.Optional;
import java.util.ResourceBundle;
import java.util.concurrent.atomic.AtomicLong;

public class HistoryController extends BaseController {
    public final ObservableList<SwapData> historyObservableList = FXCollections.synchronizedObservableList(FXCollections.observableArrayList());
    private final ObservableList<SwapData> asbHistoryObservableList = FXCollections.synchronizedObservableList(FXCollections.observableArrayList());
    private final ObservableList<PieChart.Data> swapsPieChartData = FXCollections.synchronizedObservableList(FXCollections.observableArrayList());
    private final ObservableList<PieChart.Data> asbPieChartData = FXCollections.synchronizedObservableList(FXCollections.observableArrayList());
    @FXML
    public GridPane historyPane;
    @FXML
    public HBox historyHBox;
    @FXML
    public TableView<SwapData> historyTable;
    @FXML
    public TableColumn<SwapData, String> swapIdColumn;
    @FXML
    public TableColumn<SwapData, String> dateColumn;
    @FXML
    public TableColumn<SwapData, String> statusColumn;
    @FXML
    public Button historyRefreshButton;
    @FXML
    public Label refreshHistoryText;
    @FXML
    public PieChart historyPieChart;
    @FXML
    public HBox asbHistoryHBox;
    @FXML
    public TableView<SwapData> asbHistoryTable;
    @FXML
    public TableColumn<SwapData, String> asbSwapIdColumn;
    @FXML
    public TableColumn<SwapData, String> asbDateColumn;
    @FXML
    public TableColumn<SwapData, String> asbStatusColumn;
    @FXML
    public Button asbHistoryRefreshButton;
    @FXML
    public Label asbRefreshHistoryText;
    @FXML
    public PieChart asbPieChart;
    @FXML
    public StackPane historyDonutPane;
    @FXML
    public Circle historyDonutHole;
    @FXML
    public Label historyDonutCount;
    @FXML
    public StackPane asbDonutPane;
    @FXML
    public Circle asbDonutHole;
    @FXML
    public Label asbDonutCount;
    public boolean initialized = false;

    @Override
    public void initialize(URL url, ResourceBundle rb) {
        super.initialize(url, rb);
        setHistoryTable();
        refreshHistoryList();
    }

    @Override
    protected void setupListeners() {
        historyTable.getSelectionModel().selectedItemProperty().addListener((observable, oldValue, newValue) -> {
            if (newValue != null && newValue != oldValue) {
                resumeSwap(newValue.swapId());
            }
        });
    }

    @Override
    public Button getStartButton() {
        return null;
    }

    public void setHistoryTable() {
        swapIdColumn.setCellValueFactory(param -> new SimpleStringProperty(param.getValue().swapId()));
        dateColumn.setCellValueFactory(param -> new SimpleStringProperty(param.getValue().date()));
        statusColumn.setCellValueFactory(param -> new SimpleStringProperty(param.getValue().status()));
        historyTable.setItems(historyObservableList);

        asbSwapIdColumn.setCellValueFactory(param -> new SimpleStringProperty(param.getValue().swapId()));
        asbDateColumn.setCellValueFactory(param -> new SimpleStringProperty(param.getValue().date()));
        asbStatusColumn.setCellValueFactory(param -> new SimpleStringProperty(param.getValue().status()));
        asbHistoryTable.setItems(asbHistoryObservableList);
        asbHistoryTable.setSelectionModel(null);

        asbPieChart.setData(asbPieChartData);
        historyPieChart.setData(swapsPieChartData);

        statusColumn.setCellFactory(col -> statusCell(false));
        asbStatusColumn.setCellFactory(col -> statusCell(true));
        historyTable.setPlaceholder(placeholderLabel("No swaps yet"));
        asbHistoryTable.setPlaceholder(placeholderLabel("No ASB events yet"));
        installCopySwapId(historyTable);
        installCopySwapId(asbHistoryTable);

        // Same ring ratio as the liquidity donut (radius 46 on its 160px chart,
        // which renders ~130px of pie after chart padding): keeps the hole sized
        // to the center labels at any pane size.
        historyDonutHole.radiusProperty().bind(Bindings.min(historyPieChart.widthProperty(), historyPieChart.heightProperty()).subtract(30).multiply(0.3538));
        asbDonutHole.radiusProperty().bind(Bindings.min(asbPieChart.widthProperty(), asbPieChart.heightProperty()).subtract(30).multiply(0.3538));
    }

    private static TableCell<SwapData, String> statusCell(boolean prettify) {
        return new TableCell<>() {
            @Override
            protected void updateItem(String status, boolean empty) {
                super.updateItem(status, empty);
                if (empty || status == null) {
                    setText(null);
                    setStyle("");
                    return;
                }
                setText(prettify ? prettifyAsbStatus(status) : prettifySwapStatus(status));
                setStyle("-fx-text-fill: " + pieColor(status) + ";");
            }
        };
    }

    private static Label placeholderLabel(String text) {
        Label label = new Label(text);
        label.setTextFill(Color.web("#9aa0a6"));
        return label;
    }

    private static void installCopySwapId(TableView<SwapData> table) {
        table.setRowFactory(tv -> {
            TableRow<SwapData> row = new TableRow<>();
            MenuItem copyItem = new MenuItem("Copy Swap ID");
            copyItem.setOnAction(e -> {
                if (row.getItem() == null) return;
                ClipboardContent content = new ClipboardContent();
                content.putString(row.getItem().swapId());
                Clipboard.getSystemClipboard().setContent(content);
                row.setStyle("-fx-background-color: rgba(39,190,77,0.25);");
                PauseTransition flash = new PauseTransition(Duration.seconds(1.2));
                flash.setOnFinished(ev -> row.setStyle(""));
                flash.play();
            });
            row.setContextMenu(new ContextMenu(copyItem));
            // A secondary press must not move the selection: on the client table
            // selection is what resumes a swap.
            row.addEventFilter(MouseEvent.MOUSE_PRESSED, e -> {
                if (e.getButton() == MouseButton.SECONDARY) e.consume();
            });
            return row;
        });
    }

    private static void flashRefreshIcon(Button button) {
        if (!(button.getGraphic() instanceof FontAwesomeIconView icon)) return;
        icon.setFill(Color.web("#27be4d"));
        PauseTransition flash = new PauseTransition(Duration.seconds(1.2));
        flash.setOnFinished(e -> icon.setFill(Color.WHITE));
        flash.play();
    }

    private static String tipText(String name, double count, double total) {
        String share = total > 0 ? " (" + Math.round(count * 100 / total) + "%)" : "";
        return name + ": " + (long) count + share;
    }

    static String prettifySwapStatus(String status) {
        if (status == null) return null;
        return switch (status) {
            case "BTC_LOCKED" -> "BTC locked";
            case "REFUND_PENDING" -> "Refund pending";
            case "REFUNDED" -> "Refunded";
            case "PARTIALLY_REFUNDED" -> "Partially refunded";
            case "CANCELLED" -> "Cancelled";
            default -> status;
        };
    }

    static String prettifyAsbStatus(String status) {
        return switch (status) {
            case "started", "Started" -> "Started";
            case "bitcoin lock transaction in mempool", "Bitcoin lock transaction in mempool" ->
                    "BTC lock seen (mempool)";
            case "btc is locked", "Bitcoin locked" -> "BTC locked";
            case "xmr is ready to lock", "Monero ready to lock" -> "XMR ready to lock";
            case "xmr lock transaction constructed", "Monero lock transaction constructed" ->
                    "XMR lock constructed";
            case "xmr lock transaction sent", "Monero lock transaction sent" -> "XMR lock sent";
            case "xmr lock transfer proof sent", "Monero lock transfer proof sent" ->
                    "XMR lock proof sent";
            case "xmr is locked", "Monero locked" -> "XMR locked";
            case "encrypted signature is learned", "Encrypted signature learned" ->
                    "Signature learned";
            case "bitcoin redeem transaction published", "Bitcoin redeem transaction published" ->
                    "BTC redeem published";
            case "btc is redeemed", "Done: BtcRedeemed" -> "BTC redeemed";
            case "waiting for cancel timelock expiration", "Waiting for cancel timelock to expire" ->
                    "Waiting for cancel timelock";
            case "cancel timelock is expired", "Cancel timelock is expired" -> "Cancel timelock expired";
            case "btc is cancelled", "Bitcoin cancel transaction published" -> "Cancelled";
            case "btc is punishable", "Bitcoin punishable" -> "Punishable";
            case "btc is punished", "Done: BtcPunished" -> "Punished";
            case "btc is refunded", "Done: BtcEarlyRefunded" -> "BTC refunded";
            case "btc is early refundable", "Bitcoin early refundable" -> "BTC early refundable";
            case "btc is early refunded" -> "BTC early refunded";
            case "xmr is refunded", "Monero refund transaction published", "Done: XmrRefunded" ->
                    "XMR refunded";
            case "xmr is refundable", "Monero refundable" -> "XMR refundable";
            case "xmr refund tx is constructed", "Monero refund transaction constructed" ->
                    "XMR refund constructed";
            case "xmr refund tx is published" -> "XMR refund published";
            case "btc withhold published", "Bitcoin withhold transaction published" -> "BTC withheld";
            case "btc withheld", "Done: BtcWithheld" -> "BTC withheld";
            case "btc mercy granted", "Bitcoin mercy initiated" -> "Mercy initiated";
            case "btc mercy published", "Bitcoin mercy published" -> "Mercy published";
            case "btc mercy confirmed", "Done: BtcMercyConfirmed" -> "Mercy confirmed";
            case "safely aborted", "Done: SafelyAborted" -> "Safely aborted";
            default -> status;
        };
    }

    static String canonicalAsbStatus(String status) {
        return switch (prettifyAsbStatus(status)) {
            case "BTC locked" -> "btc is locked";
            case "XMR locked" -> "xmr is locked";
            case "BTC redeemed" -> "btc is redeemed";
            case "Cancelled", "Cancel timelock expired", "Waiting for cancel timelock" ->
                    "btc is cancelled";
            case "Punished", "Punishable" -> "btc is punished";
            case "BTC refunded", "BTC early refunded", "BTC early refundable" -> "btc is refunded";
            case "XMR refunded", "XMR refundable", "XMR refund constructed", "XMR refund published" ->
                    "xmr is refunded";
            case "Safely aborted" -> "safely aborted";
            default -> status;
        };
    }

    public void refreshHistoryList() {
        refreshSwapHistoryList();
        refreshAsbHistoryList();
    }

    private void refreshSwapHistoryList() {
        GUISwap.executorService.submit(() -> {
            if (GUISwap.appSwap == null) return;
            updateGui(() -> {
                refreshHistoryText.setText("Loading history...");
            });

            GetHistoryRequest getHistoryRequest = new GetHistoryRequest(GUISwap.appSwap.getParams());
            GetHistoryResponse history = GUISwap.appSwap.getHistory(getHistoryRequest);
            historyObservableList.setAll(history.getSwaps());
            buildSwapPieChart();

            updateGui(() -> {
                refreshHistoryText.setText("");
                historyDonutPane.setMaxWidth(historyHBox.getWidth() / 3);
            });
            initialized = true;
        });
    }

    public void refreshAsbHistoryList() {
        GUISwap.executorService.submit(() -> {
            if (GUISwap.appSwap == null) return;
            updateGui(() -> {
                asbRefreshHistoryText.setText("Loading history...");
            });

            GetHistoryRequest getHistoryRequest = new GetHistoryRequest(GUISwap.appSwap.getParams());
            GetHistoryResponse asbHistory = GUISwap.appAsb.getHistory(getHistoryRequest);
            asbHistoryObservableList.setAll(asbHistory.getSwaps());
            buildAsbPieChart();

            updateGui(() -> {
                asbRefreshHistoryText.setText("");
                asbDonutPane.setMaxWidth(asbHistoryHBox.getWidth() / 3);
            });
        });
    }

    @FXML
    public void refreshSwapHistoryButton() {
        flashRefreshIcon(historyRefreshButton);
        refreshSwapHistoryList();
    }

    @FXML
    public void refreshAsbHistoryButton() {
        flashRefreshIcon(asbHistoryRefreshButton);
        refreshAsbHistoryList();
    }

    // Terminal or nothing-to-resume statuses: resuming these either cannot
    // work (a swap whose only persisted state is Started never locked
    // anything - reported as SAFELY_ABORTED) or would loop forever against a
    // finished trade. BTC_REDEEMED is deliberately ABSENT: it is not terminal
    // for bob - the XMR lock sweep still has to run.
    private static final java.util.Set<String> NON_RESUMABLE_STATUSES = java.util.Set.of(
            "SAFELY_ABORTED", "XMR_REDEEMED", "REFUNDED", "PARTIALLY_REFUNDED", "PUNISHED", "INVALID_STATE");

    public static boolean isNonResumableStatus(String status) {
        return status != null && NON_RESUMABLE_STATUSES.contains(status);
    }

    private static final java.util.Set<String> nonResumableWarned = java.util.concurrent.ConcurrentHashMap.newKeySet();

    public void resumeSwap(String swapId) {
        if (swapId != null && !swapId.isEmpty()) {
            // Zombie guard: a swap in a terminal/pre-lock state must not be
            // auto-resumed (or re-resumed on error), otherwise every unlock
            // and every error restarts a doomed swap forever. Warn once per
            // swap so refresh-driven re-invocations cannot spam the log.
            Optional<SwapData> zombie = historyObservableList.stream()
                    .filter(swapData -> swapData.swapId().equals(swapId))
                    .filter(swapData -> isNonResumableStatus(swapData.status()))
                    .findAny();
            if (zombie.isPresent()) {
                if (nonResumableWarned.add(swapId)) {
                    MainController.getInstance().printSwapLogLn(LogType.WARN,
                            "Swap " + swapId + " is already terminal (" + zombie.get().status()
                                    + "). Nothing to resume.", false);
                }
                return;
            }

            if (GUISwap.isSwapRunning()) {
                MainController.getInstance().printSwapLogLn(LogType.INFO, "Could not start swap: Another swap is already running.", false);
                return;
            }

            SwapsController activeSwapsController = MainController.getInstance().getSwapsController();
            activeSwapsController.beginSwapAction();
            activeSwapsController.setProgress(-1.0f);
            activeSwapsController.prepareSwapDisplay("Resuming swap...", "resuming");
            activeSwapsController.startResumeHeartbeat();
            boolean started = maybeStartSwapThread(() -> {
                GUISwap.appSwap.getProxy().ifPresentOrElse(proxy -> {
                    long swapsAccount = HelperSwapsDb.getInstance().getSwapsAccount(swapId);
                    SwapsWallet swapsWallet = SwapsWallet.getInstance();
                    int swapsReceiveIndex = swapsWallet.getNextReceiveIndex(WhirlpoolAccount.SWAPS_DEPOSIT, AddressType.SEGWIT_NATIVE);
                    int swapsChangeIndex = swapsWallet.getNextChangeIndex(WhirlpoolAccount.SWAPS_DEPOSIT, AddressType.SEGWIT_NATIVE);
                    ResumeRequest resumeRequest = new ResumeRequest(GUISwap.appSwap.getSeedAsBase64(), swapId, GUISwap.appSwap.getElectrumServer(), proxy, GUISwap.appSwap.getParams(), GUISwap.appSwap.getProxyPort(), swapsAccount, swapsReceiveIndex, swapsChangeIndex, HelperProperties.dojoUrl, HelperProperties.dojoApiKey);
                    GUISwap.appSwap.resume(resumeRequest);
                }, () -> activeSwapsController.failSwapStart("Tor is not ready yet, so the swap was not resumed. Wait for the Tor indicator to turn ready, then resume it again."));

            });

            if (started) {
                updateGui(() -> {
                    MainController.getInstance().printSwapLogLn(LogType.INFO, "Resuming swap: " + swapId, false);
                    MainController.getInstance().printSwapLogLn(LogType.INFO, "Please wait...", false);
                    historyTable.getSelectionModel().clearSelection();
                });
            }

            if (!started) {
                activeSwapsController.stopResumeHeartbeat();
                MainController.getInstance().printSwapLogLn(LogType.INFO, "Could not start swap: Another swap is already running.", false);
            }
        }
    }

    private void buildSwapPieChart() {
        AtomicLong countBtcLocked = new AtomicLong(0);
        AtomicLong countXmrLocked = new AtomicLong(0);
        AtomicLong countBtcRedeemed = new AtomicLong(0);
        AtomicLong countXmrRedeemed = new AtomicLong(0);
        AtomicLong countCanceled = new AtomicLong(0);
        AtomicLong countRefundPending = new AtomicLong(0);
        AtomicLong countPunished = new AtomicLong(0);
        AtomicLong countBtcRefunded = new AtomicLong(0);
        AtomicLong countPartiallyRefunded = new AtomicLong(0);
        AtomicLong countSafelyAborted = new AtomicLong(0);

        historyObservableList.forEach(swapData -> {
            switch (SwapData.Status.valueOf(swapData.status())) {
                case BTC_LOCKED -> countBtcLocked.getAndIncrement();
                case XMR_LOCKED -> countXmrLocked.getAndIncrement();
                case BTC_REDEEMED -> countBtcRedeemed.getAndIncrement();
                case XMR_REDEEMED -> countXmrRedeemed.getAndIncrement();
                case CANCELLED -> countCanceled.getAndIncrement();
                case REFUND_PENDING -> countRefundPending.getAndIncrement();
                case REFUNDED -> countBtcRefunded.getAndIncrement();
                case PARTIALLY_REFUNDED -> countPartiallyRefunded.getAndIncrement();
                case PUNISHED -> countPunished.getAndIncrement();
                case SAFELY_ABORTED -> countSafelyAborted.getAndIncrement();
            }
        });

        updateGui(() -> {
            swapsPieChartData.setAll(
                    new PieChart.Data(SwapData.Status.BTC_LOCKED.toString(), countBtcLocked.get()),
                    new PieChart.Data(SwapData.Status.XMR_LOCKED.toString(), countXmrLocked.get()),
                    new PieChart.Data(SwapData.Status.BTC_REDEEMED.toString(), countBtcRedeemed.get()),
                    new PieChart.Data(SwapData.Status.XMR_REDEEMED.toString(), countXmrRedeemed.get()),
                    new PieChart.Data(SwapData.Status.CANCELLED.toString(), countCanceled.get()),
                    new PieChart.Data(SwapData.Status.REFUND_PENDING.toString(), countRefundPending.get()),
                    new PieChart.Data(SwapData.Status.REFUNDED.toString(), countBtcRefunded.get()),
                    new PieChart.Data(SwapData.Status.PARTIALLY_REFUNDED.toString(), countPartiallyRefunded.get()),
                    new PieChart.Data(SwapData.Status.PUNISHED.toString(), countPunished.get()),
                    new PieChart.Data(SwapData.Status.SAFELY_ABORTED.toString(), countSafelyAborted.get())
            );

            double swapsTotal = swapsPieChartData.stream().mapToDouble(PieChart.Data::getPieValue).sum();
            swapsPieChartData.forEach(d -> {
                Tooltip tip = new Tooltip();
                tip.setText(tipText(d.getName(), d.getPieValue(), swapsTotal));
                Tooltip.install(d.getNode(), tip);
                d.getNode().setStyle("-fx-pie-color: " + pieColor(d.getName()) + ";");
            });
            historyDonutCount.setText(String.valueOf((long) swapsTotal));
        });
    }

    private void buildAsbPieChart() {
        final String BTC_LOCKED = "btc is locked";
        final String XMR_LOCKED = "xmr is locked";
        final String BTC_REDEEMED = "btc is redeemed";
        final String CANCELLED = "btc is cancelled";
        final String PUNISHED = "btc is punished";
        final String BTC_REFUNDED = "btc is refunded";
        final String XMR_REFUNDED = "xmr is refunded";
        final String SAFELY_ABORTED = "safely aborted";

        AtomicLong countBtcLocked = new AtomicLong(0);
        AtomicLong countXmrLocked = new AtomicLong(0);
        AtomicLong countBtcRedeemed = new AtomicLong(0);
        AtomicLong countCanceled = new AtomicLong(0);
        AtomicLong countPunished = new AtomicLong(0);
        AtomicLong countBtcRefunded = new AtomicLong(0);
        AtomicLong countXmrRefunded = new AtomicLong(0);
        AtomicLong countSafelyAborted = new AtomicLong(0);
        AtomicLong countInFlightPastLock = new AtomicLong(0);

        asbHistoryObservableList.forEach(swapData -> {
            // States after the BTC lock that the pie has no slice for (proof sent,
            // redeem published, ...) still count as active work.
            if (AsbActivityStats.isInFlightPastLock(canonicalAsbStatus(swapData.status()))) {
                countInFlightPastLock.getAndIncrement();
            }
            switch (canonicalAsbStatus(swapData.status())) {
                case BTC_LOCKED -> countBtcLocked.getAndIncrement();
                case XMR_LOCKED -> countXmrLocked.getAndIncrement();
                case BTC_REDEEMED -> countBtcRedeemed.getAndIncrement();
                case CANCELLED -> countCanceled.getAndIncrement();
                case PUNISHED -> countPunished.getAndIncrement();
                case BTC_REFUNDED -> countBtcRefunded.getAndIncrement();
                case XMR_REFUNDED -> countXmrRefunded.getAndIncrement();
                case SAFELY_ABORTED -> countSafelyAborted.getAndIncrement();
            }
        });

        updateGui(() -> {
            asbPieChartData.setAll(
                    new PieChart.Data(BTC_LOCKED, countBtcLocked.get()),
                    new PieChart.Data(XMR_LOCKED, countXmrLocked.get()),
                    new PieChart.Data(BTC_REDEEMED, countBtcRedeemed.get()),
                    new PieChart.Data(CANCELLED, countCanceled.get()),
                    new PieChart.Data(PUNISHED, countPunished.get()),
                    new PieChart.Data(BTC_REFUNDED, countBtcRefunded.get()),
                    new PieChart.Data(XMR_REFUNDED, countXmrRefunded.get()),
                    new PieChart.Data(SAFELY_ABORTED, countSafelyAborted.get())
            );

            double asbTotal = asbPieChartData.stream().mapToDouble(PieChart.Data::getPieValue).sum();
            asbPieChartData.forEach(d -> {
                Tooltip tip = new Tooltip();
                tip.setText(tipText(prettifyAsbStatus(d.getName()), d.getPieValue(), asbTotal));
                Tooltip.install(d.getNode(), tip);
                d.getNode().setStyle("-fx-pie-color: " + pieColor(d.getName()) + ";");
            });
            asbDonutCount.setText(String.valueOf((long) asbTotal));
        });

        // Glanceable counts for the Liquidity dashboard's activity strip; the
        // full table + status pie stay on this page (no duplication).
        long total = asbHistoryObservableList.size();
        long completed = countBtcRedeemed.get() + countPunished.get();
        long active = AsbActivityStats.active(countBtcLocked.get(), countXmrLocked.get(), countInFlightPastLock.get());
        long settled = completed + Math.max(0, total - completed - active);
        int successPercent = settled > 0 ? (int) Math.round(completed * 100.0 / settled) : 0;
        LiquidityController liquidityController = LiquidityController.getInstance();
        if (liquidityController != null)
            liquidityController.updateAsbActivity(total, completed, active, successPercent);
    }

    private static String pieColor(String status) {
        String name = canonicalAsbStatus(status).toUpperCase();
        if (name.contains("REDEEM")) return "#8f1a14";
        if (name.contains("XMR REFUND")) return "#7a4d9e"; // distinct from BTC refund grey: a swap can have BOTH slices
        if (name.contains("LOCK")) return "#be7327";
        if (name.contains("CANCEL")) return "#8a8f96";
        if (name.contains("REFUND")) return "#4d5259";
        if (name.contains("PUNISH")) return "#7b1a1a";
        return "#b9bec6";
    }

    public boolean checkIncompleteSwaps() {
        Optional<SwapData> firstIncompleteSwap = historyObservableList.stream().filter(swapData -> {
            String status = swapData.status();
            return !status.equals("XMR_REDEEMED") && !status.equals("BTC_REDEEMED") && !status.equals("REFUNDED") && !status.equals("PARTIALLY_REFUNDED") && !status.equals("PUNISHED") && !status.equals("SAFELY_ABORTED");
        }).findAny();

        if (firstIncompleteSwap.isPresent()) {
            MainController.getInstance().printSwapLogLn(LogType.HIGHLIGHT, "Incomplete swap found: " + firstIncompleteSwap.get().swapId() + ". Please resume first before starting a new swap.", false);
            return true;
        } else {
            return false;
        }
    }
}
