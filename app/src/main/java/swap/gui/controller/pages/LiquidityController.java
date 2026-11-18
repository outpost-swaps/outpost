package swap.gui.controller.pages;

import de.jensd.fx.glyphs.fontawesome.FontAwesomeIconView;
import javafx.animation.PauseTransition;
import javafx.collections.FXCollections;
import javafx.fxml.FXML;
import javafx.scene.chart.PieChart;
import javafx.scene.control.Button;
import javafx.scene.control.Label;
import javafx.scene.control.ProgressIndicator;
import javafx.scene.control.TextField;
import javafx.scene.control.Tooltip;
import javafx.scene.image.Image;
import javafx.scene.image.ImageView;
import javafx.scene.input.Clipboard;
import javafx.scene.input.ClipboardContent;
import javafx.scene.layout.GridPane;
import javafx.scene.layout.VBox;
import javafx.scene.paint.Color;
import javafx.scene.shape.Circle;
import javafx.util.Duration;
import org.bitcoinj.core.Coin;
import swap.gui.GUISwap;
import swap.gui.controller.BaseController;
import swap.gui.controller.MainController;
import swap.gui.image.AddressQrCode;
import swap.lib.AppAsb;
import swap.model.AsbBtcBalanceData;
import swap.model.AsbInitData;

import java.math.BigDecimal;
import java.net.URL;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.ResourceBundle;

public class LiquidityController extends BaseController {
    public static LiquidityController instance = null;
    @FXML
    public GridPane liquidityPane;
    @FXML
    public VBox offlineBox;
    @FXML
    public javafx.scene.control.Button startAsbButton;
    @FXML
    public VBox progressBox;
    @FXML
    public ProgressIndicator progressIndicator;
    @FXML
    public Label startupStatus;
    @FXML
    public VBox asbBox;
    // Status strip
    @FXML
    public Circle statusDot;
    @FXML
    public Label statusText;
    @FXML
    public Label rendezvousText;
    @FXML
    public Label peerIdShort;
    // Balances card
    @FXML
    public Label unlockedXmr;
    @FXML
    public Label unlockedBtcEquiv;
    @FXML
    public Label btcBalanceLarge;
    @FXML
    public Label btcXmrEquiv;
    // Liquidity donut card
    @FXML
    public PieChart liquidityDonut;
    @FXML
    public Label xmrBalanceLarge;
    @FXML
    public Label availableXmrLabel;
    @FXML
    public Label reservedXmrLabel;
    @FXML
    public Label lockedXmr;
    // Market card
    @FXML
    public Label spotXmrPrice;
    @FXML
    public Label feeChip;
    @FXML
    public TextField asbPrice;
    @FXML
    public TextField minBtc;
    @FXML
    public TextField maxBtc;
    // Identity card
    @FXML
    public TextField externalAddress;
    @FXML
    public TextField xmrAddress;
    @FXML
    public Button copyMultiaddrButton;
    @FXML
    public Button copyXmrAddressButton;
    @FXML
    public ImageView xmrQR;
    // Activity strip
    @FXML
    public Label activityTotal;
    @FXML
    public Label activityCompleted;
    @FXML
    public Label activityActive;
    @FXML
    public Label activitySuccess;
    @FXML
    public Label activityEarned;
    @FXML
    public Label activitySold;
    // Live maker swap strip
    @FXML
    public Circle liveSwapDot;
    @FXML
    public Label liveSwapText;
    @FXML
    public Label liveSwapCount;
    @FXML
    public Label swapsInProgressText;
    @FXML
    public javafx.scene.layout.VBox inProgressList;

    // Reservation accounting mirrored from the ASB feed: RESERVED/RERESERVED
    // add, RELEASED subtracts. The armed-guard release on a failed handshake
    // is silent (no event), so this can drift high until the next terminal
    // release or an ASB restart - acceptable for a dashboard.
    private BigDecimal reservedXmr = BigDecimal.ZERO;
    // All-time gross BTC earned, persisted (swap id -> satoshis) so it survives restarts.
    private swap.helper.AsbEarnings earnings = null;
    private Coin spotPrice = Coin.ZERO;
    private Coin btcBalance = Coin.ZERO;
    private boolean ourRateKnown = false;
    private boolean formattingLocked = false;
    private boolean formattingCenter = false;
    // Live maker-swap strip: fed by ProcessAsb's parsed event stream. A
    // single-slot-per-swap map keeps only in-flight swaps; terminal events
    // remove them. The persisted Activity counts remain the history source.
    private final Map<String, String> activeAsbSwaps = new LinkedHashMap<>();
    private final Tooltip liveSwapTooltip = new Tooltip();
    private boolean sawLiveAsbState = false;
    private long historyActiveSwaps = 0;

    @Override
    public void initialize(URL url, ResourceBundle rb) {
        instance = this;
        Tooltip.install(activityEarned, new Tooltip("All-time gross BTC your ASB has received from swaps (before the cost of the XMR it paid out)"));
        if (activitySold != null) {
            Tooltip.install(activitySold, new Tooltip("All-time XMR your ASB has sold in completed swaps"));
        }
        Tooltip.install(spotXmrPrice, new Tooltip("Spot XMR/BTC price from moneropricenow.com (via the price ticker feed)"));
        Tooltip.install(asbPrice, new Tooltip("Your advertised price per 1 XMR (spot + your fee)"));
        Tooltip.install(feeChip, new Tooltip("Your markup over the spot price"));
        Tooltip.install(availableXmrLabel, new Tooltip("XMR free to back new swap quotes"));
        Tooltip.install(reservedXmrLabel, new Tooltip("XMR promised to in-flight swap quotes; released when the swap ends"));
        Tooltip.install(lockedXmr, new Tooltip("XMR locked in the Monero wallet (in-progress transactions)"));
        Tooltip.install(liveSwapText, liveSwapTooltip);
        renderAsbSwaps(); // "No swaps in progress" from the start, not only after the first event
        super.initialize(url, rb);

        SwapsController swapsController = SwapsController.instance;
        if (swapsController != null && swapsController.getCurrentXmrPriceInBtc().value > 0) {
            setSpotPrice(swapsController.getCurrentXmrPriceInBtc());
        }
        renderAsbSwaps();
    }

    @Override
    protected void setupListeners() {
        unlockedXmr.textProperty().addListener((observable, oldValue, newValue) -> updateDonut());

        lockedXmr.textProperty().addListener((observable, oldValue, newValue) -> {
            if (!formattingLocked) {
                formattingLocked = true;
                try {
                    lockedXmr.setText(fmtXmr(parseXmr(newValue)));
                } finally {
                    formattingLocked = false;
                }
            }
            updateDonut();
        });

        // The donut center shows the total without its " XMR" suffix.
        xmrBalanceLarge.textProperty().addListener((observable, oldValue, newValue) -> {
            if (formattingCenter) return;
            formattingCenter = true;
            try {
                xmrBalanceLarge.setText(fmtXmr(parseXmr(newValue)));
            } finally {
                formattingCenter = false;
            }
        });
    }

    public static LiquidityController getInstance() {
        return instance;
    }

    public void setupAsb(AsbInitData data, double unlockedXmrBalance, double lockedXmrBalance) {
        updateGui(() -> {
            btcBalanceLarge.setTooltip(new Tooltip("SWAPS_ASB BTC Balance"));
            externalAddress.setText(data.multiaddr());
            externalAddress.setTooltip(new Tooltip("Seller Address"));
            xmrAddress.setText(data.moneroAddress());
            Tooltip tooltip = new Tooltip("XMR Address");
            tooltip.setStyle("-fx-font-size: 14");
            xmrAddress.setTooltip(tooltip);

            try {
                Image addressQrCode = new AddressQrCode(data.moneroAddress()).generateXmr();
                xmrQR.setImage(addressQrCode);
                Tooltip.install(xmrQR, new Tooltip("XMR Address"));
            } catch (Exception e) {
                System.err.println("Error generating QR Image: " + e.getMessage());
            }

            unlockedXmr.setText(fmtXmr(unlockedXmrBalance));
            lockedXmr.setText(String.valueOf(lockedXmrBalance));
            loadEarnings();

            Tooltip tt = new Tooltip("Set on Settings page");
            Tooltip.install(minBtc, tt);
            Tooltip.install(maxBtc, tt);

            // Populate min/max/fee from local config immediately so user sees
            // something before the network seller refresh fills in advertised values.
            minBtc.setText(swap.lib.AppAsb.getMinQuantity().toPlainString());
            maxBtc.setText(swap.lib.AppAsb.getMaxQuantity().toPlainString());
            updateFeeChip();
            updateYourRateEstimate();

            // update sellers list & min/max/price on Liquidity page
            MainController.getInstance().getSwapsController().onRefreshSellersButtonClick();

            statusText.setText("ASB Online");
            statusDot.setFill(Color.web("#27be4d"));
            peerIdShort.setText(shortenPeerId(data.peerId()));
            peerIdShort.setTooltip(new Tooltip(data.peerId()));
            try {
                int rendezvous = GUISwap.appSwap != null ? GUISwap.appSwap.getRendezvousPeers().size() : 0;
                rendezvousText.setText("Rendezvous: " + rendezvous);
            } catch (Exception e) {
                rendezvousText.setText("Rendezvous: —");
            }

            setProgress(0.0f);
            renderAsbSwaps();
        });
    }

    public void onBtcBalanceData(String accountName, AsbBtcBalanceData data) {
        updateGui(() -> {
            MainController mainController = MainController.getInstance();
            if (mainController == null) return;
            if (!data.error().isEmpty()) {
                if ("SWAPS_ASB".equals(accountName)) btcBalanceLarge.setText("—");
                return;
            }
            if ("SWAPS_ASB".equals(accountName)) {
                btcBalance = data.balance();
                String balance = String.format("%.8f", data.balance().value / 100_000_000.0);
                btcBalanceLarge.setText(balance);
                updateBtcEquiv();
            }
        });
    }

    public void setSpotPrice(Coin priceInBtc) {
        this.spotPrice = priceInBtc;
        updateGui(() -> {
            spotXmrPrice.setText(String.format("%.8f", priceInBtc.value / 100_000_000.0) + " BTC");
            updateYourRateEstimate();
            updateFeeChip();
            updateBtcEquiv();
            updateDonut();
        });
    }

    public void setOurRate(double rate) {
        updateGui(() -> {
            ourRateKnown = true;
            asbPrice.setText(rate + " BTC");
        });
    }

    // Fill YOUR RATE immediately from spot + fee until the sellers refresh
    // returns the advertised price (which is the same math, computed ASB-side).
    // With a manual rate the advertised price IS the manual rate, so show that.
    private void updateYourRateEstimate() {
        if (ourRateKnown) return;
        Coin manual = AppAsb.getManualPrice();
        if (manual != null) {
            asbPrice.setText(manual.toPlainString() + " BTC");
            Tooltip.install(asbPrice, new Tooltip("Your advertised price per 1 XMR (manual rate, advertised as-is)"));
            return;
        }
        if (spotPrice == null || spotPrice.value <= 0) return;
        long rate = Math.round(spotPrice.value * (1.0 + AppAsb.getFee() / 100.0));
        asbPrice.setText(Coin.valueOf(rate).toPlainString() + " BTC");
    }

    // FEE shows the configured Rate Fee — or, when a manual rate is set, the
    // markup that rate actually implies over spot (the configured fee is
    // dormant in manual mode). Recalculated whenever spot moves.
    private void updateFeeChip() {
        Coin manual = AppAsb.getManualPrice();
        if (manual == null) {
            feeChip.setText(String.format("%.2f%%", AppAsb.getFee()));
            Tooltip.install(feeChip, new Tooltip("Your markup over the spot price"));
            return;
        }
        if (spotPrice == null || spotPrice.value <= 0) {
            feeChip.setText("—");
        } else {
            double effectiveFee = ((double) manual.value / spotPrice.value - 1.0) * 100.0;
            feeChip.setText(String.format("%.2f%%", effectiveFee));
        }
        Tooltip.install(feeChip, new Tooltip("Recalculated from your manual rate vs the spot price — the configured Rate Fee is inactive"));
    }

    private void updateBtcEquiv() {
        if (spotPrice == null || spotPrice.value <= 0) {
            btcXmrEquiv.setText("≈ — XMR");
            return;
        }
        double xmr = btcBalance.value / (double) spotPrice.value;
        btcXmrEquiv.setText(String.format("≈ %.6f XMR", xmr));
    }

    public void onLiquidityReserved(String amount) {
        reservedXmr = reservedXmr.add(parseAmount(amount));
        updateDonut();
    }

    public void onLiquidityReleased(String amount) {
        reservedXmr = reservedXmr.subtract(parseAmount(amount));
        if (reservedXmr.signum() < 0) reservedXmr = BigDecimal.ZERO;
        updateDonut();
    }

    /**
     * Loads the persisted all-time total, once backfilling it from the ASB log
     * for swaps that were earned before totals were kept.
     */
    private void loadEarnings() {
        try {
            earnings = swap.helper.AsbEarnings.load();
            if (earnings.needsBackfill()) {
                java.io.File log = new java.io.File(swap.lib.AppSwap.getSwapRootDir(), "logs/asb.log");
                if (log.isFile()) {
                    try (java.util.stream.Stream<String> lines = java.nio.file.Files.lines(log.toPath())) {
                        java.util.List<String> earned = lines.filter(line -> line.contains("ASB_EARNED_BTC") || line.contains("ASB_LIQUIDITY_RESERVED")
                                        || line.contains("ASB_LIQUIDITY_RERESERVED")).toList();
                        swap.helper.AsbEarnings.parseEarnedLines(earned)
                                .forEach((swapId, entry) -> earnings.record(swapId, entry.sats(), entry.xmrPico()));
                    }
                }
                earnings.markBackfilled();
                earnings.save();
            }
        } catch (Exception e) {
            System.err.println("[EARNED] could not load the earnings total: " + e.getMessage());
        }
        showEarned();
    }

    private void showEarned() {
        long total = earnings == null ? 0 : earnings.totalSatoshis();
        long xmr = earnings == null ? 0 : earnings.totalXmrPico();
        updateGui(() -> {
            activityEarned.setText(Coin.valueOf(total).toPlainString() + " BTC");
            if (activitySold != null) {
                activitySold.setText(swap.helper.AsbEarnings.formatXmr(xmr) + " XMR");
            }
        });
    }

    public void onBtcEarned(String swapId, long satoshis, long xmrPico) {
        if (earnings == null) loadEarnings();
        if (earnings != null && earnings.record(swapId, satoshis, xmrPico)) {
            try {
                earnings.save();
            } catch (Exception e) {
                System.err.println("[EARNED] could not save the earnings total: " + e.getMessage());
            }
        }
        showEarned();
    }

    /**
     * Update one maker swap's live state from the ASB event stream. A later
     * event for the same swap replaces the state and moves it to the front
     * of the summary (most recent activity wins).
     */
    public void onAsbSwapState(String swapId, String state) {
        if (swapId == null || swapId.isBlank() || "unknown".equals(swapId)
                || state == null || state.isBlank()) return;
        updateGui(() -> {
            sawLiveAsbState = true;
            activeAsbSwaps.remove(swapId);
            activeAsbSwaps.put(swapId, state);
            renderAsbSwaps();
        });
    }

    /** Terminal ASB event: the swap is no longer in flight. */
    public void onAsbSwapTerminal(String swapId) {
        if (swapId == null || swapId.isBlank() || "unknown".equals(swapId)) return;
        updateGui(() -> {
            sawLiveAsbState = true;
            activeAsbSwaps.remove(swapId);
            renderAsbSwaps();
        });
    }

    /**
     * Called when a new ASB process starts or dies: stale live states from
     * the previous process must not linger in the strip.
     */
    public void clearAsbSwaps() {
        updateGui(() -> {
            activeAsbSwaps.clear();
            sawLiveAsbState = false;
            renderAsbSwaps();
        });
    }

    /** The status-bar text for the number of ASB swaps in progress. */
    static String swapsInProgressLabel(long count, boolean syncing) {
        return syncing ? "Swaps in progress: " + count + " (syncing)" : "Swaps in progress: " + count;
    }

    /**
     * The one line the Liquidity card has room for: the newest swap, and "+N more" for the rest
     * (the hover lists them all). Index 0 is the line, index 1 the "+N more" text or empty.
     */
    static String[] inProgressOneLine(List<Map.Entry<String, String>> swaps) {
        List<String> lines = inProgressLines(swaps, 1);
        String more = lines.size() > 1 ? lines.get(1) : "";
        return new String[]{lines.get(0), more};
    }

    /** The Liquidity card's "In progress" lines: newest first, at most {@code max}, then "+N more". */
    static List<String> inProgressLines(List<Map.Entry<String, String>> swaps, int max) {
        List<String> lines = new ArrayList<>();
        if (swaps.isEmpty()) {
            lines.add("No swaps in progress");
            return lines;
        }
        for (int i = swaps.size() - 1; i >= 0 && lines.size() < max; i--) {
            lines.add(shortSwapId(swaps.get(i).getKey()) + "  " + swaps.get(i).getValue());
        }
        if (swaps.size() > max) {
            lines.add("+" + (swaps.size() - max) + " more");
        }
        return lines;
    }

    /** Status bar count and the Liquidity card list, from the same live ASB swap states. */
    private void renderInProgress(List<Map.Entry<String, String>> swaps, boolean syncing) {
        if (swapsInProgressText != null) {
            long count = syncing ? historyActiveSwaps : swaps.size();
            swapsInProgressText.setText(swapsInProgressLabel(count, syncing));
            swapsInProgressText.setTextFill(Color.web(count > 0 ? "#be7327" : "#9aa0a6"));
            Tooltip.install(swapsInProgressText, liveSwapTooltip);
        }
        if (inProgressList != null) {
            inProgressList.getChildren().clear();
            String[] parts = syncing ? new String[]{"State syncing...", ""} : inProgressOneLine(swaps);
            Label first = new Label(parts[0]);
            first.setTextFill(Color.web(swaps.isEmpty() && !syncing ? "#5c6066" : "#e7eaf0"));
            first.setFont(javafx.scene.text.Font.font("Menlo", 13));
            first.setMaxWidth(Double.MAX_VALUE);
            first.setTextOverrun(javafx.scene.control.OverrunStyle.ELLIPSIS);
            javafx.scene.layout.Region spacer = new javafx.scene.layout.Region();
            javafx.scene.layout.HBox.setHgrow(spacer, javafx.scene.layout.Priority.ALWAYS);
            javafx.scene.layout.HBox row = new javafx.scene.layout.HBox(8, first, spacer);
            row.setAlignment(javafx.geometry.Pos.CENTER_LEFT);
            if (!parts[1].isEmpty()) {
                Label more = new Label(parts[1]);
                more.setTextFill(Color.web("#be7327"));
                more.setFont(javafx.scene.text.Font.font("Menlo", 12));
                more.setUnderline(true);
                more.setCursor(javafx.scene.Cursor.HAND);
                more.setTooltip(new Tooltip("Show all swaps in progress"));
                List<Map.Entry<String, String>> all = new ArrayList<>(swaps);
                more.setOnMouseClicked(event -> swap.gui.controller.popups.PopupSwapsInProgress.show(
                        more.getScene() == null ? null : (javafx.stage.Stage) more.getScene().getWindow(), all));
                row.getChildren().add(more);
            }
            // every swap with its step, one per line, on hover
            Tooltip.install(row, new Tooltip(String.join("\n", syncing ? List.of("State syncing...") : inProgressLines(swaps, Integer.MAX_VALUE))));
            inProgressList.getChildren().add(row);
        }
    }

    private void renderAsbSwaps() {
        List<Map.Entry<String, String>> current = new ArrayList<>(activeAsbSwaps.entrySet());
        renderInProgress(current, current.isEmpty() && !sawLiveAsbState && historyActiveSwaps > 0);
        if (liveSwapText == null || liveSwapDot == null || liveSwapCount == null) return;

        List<Map.Entry<String, String>> swaps = new ArrayList<>(activeAsbSwaps.entrySet());
        if (swaps.isEmpty()) {
            if (!sawLiveAsbState && historyActiveSwaps > 0) {
                // The persisted history knows about an active swap but the
                // live feed has not emitted since this ASB process started.
                liveSwapDot.setFill(Color.web("#be7327"));
                liveSwapText.setText("State syncing...");
                liveSwapCount.setText(String.valueOf(historyActiveSwaps));
                liveSwapTooltip.setText("Persisted ASB activity reports active swaps; waiting for the live event stream.");
            } else {
                liveSwapDot.setFill(Color.web("#4d5259"));
                liveSwapText.setText("No swaps in progress");
                liveSwapCount.setText("0");
                liveSwapTooltip.setText("No maker swaps are active.");
            }
            return;
        }

        Map.Entry<String, String> latest = swaps.get(swaps.size() - 1);
        int count = swaps.size();
        StringBuilder summary = new StringBuilder()
                .append(shortSwapId(latest.getKey())).append(" · ").append(latest.getValue());
        if (count > 1) summary.append(" · +").append(count - 1).append(" more");

        liveSwapDot.setFill(Color.web("#be7327"));
        liveSwapText.setText(summary.toString());
        liveSwapCount.setText(String.valueOf(count));

        StringBuilder details = new StringBuilder("Live maker swaps:");
        for (int i = swaps.size() - 1; i >= 0; i--) {
            details.append('\n').append(swaps.get(i).getKey()).append(" · ").append(swaps.get(i).getValue());
        }
        liveSwapTooltip.setText(details.toString());
    }

    private static String shortSwapId(String swapId) {
        if (swapId == null || swapId.length() <= 8) return swapId;
        return swapId.substring(0, 8) + "…";
    }

    public void updateAsbActivity(long total, long completed, long active, int successPercent) {
        updateGui(() -> {
            activityTotal.setText(String.valueOf(total));
            activityCompleted.setText(String.valueOf(completed));
            activityActive.setText(String.valueOf(active));
            activitySuccess.setText(successPercent + "%");
            this.historyActiveSwaps = active;
            renderAsbSwaps();
        });
    }

    /** The ASB panel's "Start ASB" button: starts it without a restart of the app. */
    @FXML
    public void onStartAsbClick() {
        startAsbButton.setDisable(true);
        MainController.getInstance().startAsbNow();
    }

    /** The start did not work: back to the offline panel, with the button usable again. */
    public void onAsbStartFailed() {
        updateGui(() -> {
            progressBox.setVisible(false);
            progressBox.setManaged(false);
            offlineBox.setVisible(true);
            offlineBox.setManaged(true);
            startAsbButton.setDisable(false);
        });
    }

    public void prepareAsb() {
        offlineBox.setVisible(false);
        offlineBox.setManaged(false);
        progressBox.setVisible(true);
        progressBox.setManaged(true);

        setProgress(-1.0f);
        Tooltip.install(progressIndicator, new Tooltip("Initializing..."));
        setStartupStatus("Starting the ASB...");
    }

    /** The line under the start-up spinner: what the ASB is doing, so a long first sync does not look stuck. */
    public void setStartupStatus(String text) {
        updateGui(() -> {
            if (startupStatus != null) {
                startupStatus.setText(text == null ? "" : text);
            }
        });
    }

    public void setProgress(float progress) {
        updateGui(() -> {
            progressIndicator.setProgress(progress);

            if (progress == 0.0f) {
                progressBox.setVisible(false);
                progressBox.setManaged(false);
                asbBox.setVisible(true);
                asbBox.setManaged(true);
            }
        });
    }

    public void updateAsbState() {
        updateGui(() -> {
            if (GUISwap.isAsbRunning()) {
                offlineBox.setVisible(false);
                offlineBox.setManaged(false);
                progressBox.setVisible(false);
                progressBox.setManaged(false);
                asbBox.setVisible(true);
                asbBox.setManaged(true);
            } else if (!progressBox.isVisible()) {
                offlineBox.setVisible(true);
                offlineBox.setManaged(true);
                progressBox.setVisible(false);
                progressBox.setManaged(false);
                asbBox.setVisible(false);
                asbBox.setManaged(false);
            }
        });
    }

    @FXML
    public void onCopyXmrAddress() {
        copyToClipboard(xmrAddress.getText());
        flashCopyButton(copyXmrAddressButton);
    }

    @FXML
    public void onCopyMultiaddr() {
        copyToClipboard(externalAddress.getText());
        flashCopyButton(copyMultiaddrButton);
    }

    private void flashCopyButton(Button button) {
        if (!(button.getGraphic() instanceof FontAwesomeIconView icon)) return;
        icon.setGlyphName("CHECK");
        icon.setFill(Color.web("#27be4d"));
        PauseTransition pause = new PauseTransition(Duration.millis(1200));
        pause.setOnFinished(e -> {
            icon.setGlyphName("COPY");
            icon.setFill(Color.web("#9aa0a6"));
        });
        pause.play();
    }

    private void copyToClipboard(String text) {
        if (text == null || text.isBlank()) return;
        ClipboardContent content = new ClipboardContent();
        content.putString(text);
        Clipboard.getSystemClipboard().setContent(content);
    }

    private void updateDonut() {
        updateGui(() -> {
            double unlocked = parseXmr(unlockedXmr.getText());
            double locked = parseXmr(lockedXmr.getText());
            double reserved = reservedXmr.doubleValue();
            double available = Math.max(0, unlocked - reserved);

            liquidityDonut.setData(FXCollections.observableArrayList(
                    new PieChart.Data("Available", available),
                    new PieChart.Data("Reserved", reserved),
                    new PieChart.Data("Locked", locked)));

            availableXmrLabel.setText(fmtXmr(available));
            reservedXmrLabel.setText(fmtXmr(reserved));

            if (spotPrice != null && spotPrice.value > 0) {
                Coin equivalent = Coin.valueOf(Math.round(unlocked * spotPrice.value));
                unlockedBtcEquiv.setText("≈ " + equivalent.toPlainString() + " BTC");
            } else {
                unlockedBtcEquiv.setText("≈ — BTC");
            }
        });
    }

    private static BigDecimal parseAmount(String amount) {
        if (amount == null) return BigDecimal.ZERO;
        try {
            return new BigDecimal(amount.trim().split("\\s+")[0]);
        } catch (Exception e) {
            return BigDecimal.ZERO;
        }
    }

    private static double parseXmr(String text) {
        if (text == null) return 0;
        try {
            return Double.parseDouble(text.replace("XMR", "").trim());
        } catch (Exception e) {
            return 0;
        }
    }

    /** Every XMR figure on this page: 6 decimals, rounded down (never shows more than there is). */
    public static String fmtXmr(double value) {
        return new java.math.BigDecimal(Double.toString(value)).setScale(6, java.math.RoundingMode.DOWN).toPlainString();
    }

    private static String shortenPeerId(String peerId) {
        if (peerId == null || peerId.length() <= 18) return peerId;
        return peerId.substring(0, 12) + "…" + peerId.substring(peerId.length() - 4);
    }

    @Override
    public Button getStartButton() {
        return null;
    }
}
