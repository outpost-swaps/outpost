package swap.gui.controller.pages;

import com.samourai.wallet.api.backend.beans.UnspentOutput;
import com.samourai.wallet.hd.AddressType;
import com.samourai.whirlpool.client.wallet.beans.WhirlpoolAccount;
import javafx.beans.property.ReadOnlyStringWrapper;
import javafx.collections.FXCollections;
import javafx.collections.ObservableList;
import javafx.event.ActionEvent;
import javafx.application.Platform;
import javafx.fxml.FXML;
import javafx.geometry.Insets;
import javafx.scene.Scene;
import javafx.scene.control.Button;
import javafx.scene.control.ComboBox;
import javafx.scene.control.Label;
import javafx.scene.control.TableColumn;
import javafx.scene.control.TableView;
import javafx.scene.control.TextField;
import javafx.scene.control.cell.CheckBoxTableCell;
import javafx.scene.layout.GridPane;
import javafx.scene.layout.VBox;
import javafx.scene.paint.Color;
import javafx.scene.text.Font;
import javafx.stage.Stage;
import org.bitcoinj.core.Address;
import org.bitcoinj.core.Coin;
import swap.gui.GUISwap;
import swap.gui.controller.BaseController;
import swap.gui.controller.MainController;
import swap.helper.HelperAddress;
import swap.model.LogType;
import swap.model.ScreenType;
import swap.model.UtxoRow;

import java.util.ArrayList;
import java.util.Map;
import java.util.Collection;
import java.util.HashMap;
import java.util.List;
import java.util.concurrent.TimeUnit;

public class WithdrawController extends BaseController {
    @FXML
    public GridPane withdrawPane;
    @FXML
    public TextField btcAddressTextField;
    @FXML
    public TextField btcAmountTextField;
    @FXML
    public TextField btcFeeTextField;
    @FXML
    public ComboBox<String> whirlpoolAccount;
    @FXML
    public Label selectedAccountBalance;
    @FXML
    public Button withdrawButton;
    @FXML
    public Button sendAllButton;
    @FXML
    public Button feeEstimate99Button;
    @FXML
    public Button feeEstimate50Button;
    @FXML
    public Button feeEstimate10Button;
    // The "manual (N sat/vB)" button to the right of the estimates: its label
    // mirrors the fee field live; clicking it marks manual mode selected and
    // focuses the field (the field itself holds the value).
    @FXML
    public Button manualFeeButton;
    // Currently highlighted fee source (99%/50%/10%/manual). 99% is the
    // default selection and the rate the field auto-fills from.
    private Button selectedFeeButton;
    // All-accounts balance summary bar
    @FXML public Label balDepositSummary;
    @FXML public Label balPostmixSummary;
    @FXML public Label balBadbankSummary;
    @FXML public Label balSwapsDepositSummary;
    @FXML public Label balSwapsRefundsSummary;
    @FXML public Label balSwapsAsbSummary;

    // UTXO selection table
    @FXML public TableView<UtxoRow> utxoTable;
    @FXML public TableColumn<UtxoRow, Boolean> utxoSelectColumn;
    @FXML public TableColumn<UtxoRow, String> utxoTxidColumn;
    @FXML public TableColumn<UtxoRow, String> utxoVoutColumn;
    @FXML public TableColumn<UtxoRow, String> utxoValueColumn;
    @FXML public TableColumn<UtxoRow, String> utxoConfColumn;
    @FXML public TableColumn<UtxoRow, String> utxoAddrColumn;
    @FXML public Button utxoSelectAllButton;
    @FXML public Button utxoClearButton;
    @FXML public Label utxoSelectionSummary;

    private final HashMap<String, WhirlpoolAccount> wallets = new HashMap<>();
    private final ObservableList<UtxoRow> utxoRows = FXCollections.observableArrayList();
    // Send-all (sweep) mode: the amount field is bypassed and prepareWithdraw is
    // called with Long.MAX_VALUE, which sweeps exactly the ticked UTXOs (or the
    // whole account when none are ticked) into a single output, fee deducted.
    private volatile boolean sendAllMode = false;
    // Guards against a stale refreshUtxoTable() fetch (from an account you already switched
    // away from) completing after a newer one and silently overwriting the table with the
    // wrong account's UTXOs under the new account's label.
    private final java.util.concurrent.atomic.AtomicLong utxoRefreshGeneration = new java.util.concurrent.atomic.AtomicLong(0);
    // Number of account-balance Dojo fetches currently in flight; the periodic
    // summary refresh skips its tick while any are outstanding.
    private final java.util.concurrent.atomic.AtomicInteger outstandingBalanceFetches = new java.util.concurrent.atomic.AtomicInteger(0);

    @Override
    public Button getStartButton() {
        return withdrawButton;
    }

    @Override
    public ScreenType getScreenType() {
        return ScreenType.WITHDRAW;
    }

    @Override
    protected void setupListeners() {
        wallets.put("SWAPS_ASB", WhirlpoolAccount.ASB);
        wallets.put("SWAPS_DEPOSIT", WhirlpoolAccount.SWAPS_DEPOSIT);
        wallets.put("SWAPS_REFUNDS", WhirlpoolAccount.SWAPS_REFUNDS);
        wallets.put("BADBANK (P2WPKH)", WhirlpoolAccount.BADBANK);
        List<String> orderedAccounts = List.of(
                "SWAPS_DEPOSIT", "SWAPS_REFUNDS", "SWAPS_ASB", "BADBANK (P2WPKH)");
        ObservableList<String> accounts = FXCollections.observableArrayList(orderedAccounts);
        whirlpoolAccount.setItems(accounts);
        whirlpoolAccount.setValue("SWAPS_REFUNDS");

        setupUtxoTable();

        // Refresh balance label + UTXO list whenever the source account changes.
        whirlpoolAccount.valueProperty().addListener((obs, oldVal, newVal) -> {
            utxoRows.clear();
            utxoSelectionSummary.setText("loading…");
            selectedAccountBalance.setText("Balance: fetching…");
            refreshUtxoTable(newVal);
            if (sendAllMode) updateSendAllText();
            fetchFeeEstimate();
        });
        utxoRows.clear();
        utxoSelectionSummary.setText("loading…");
        selectedAccountBalance.setText("Balance: fetching…");
        refreshUtxoTable(whirlpoolAccount.getValue());
        GUISwap.scheduledExecutorService.schedule(
                this::refreshAllAccountBalances, 10, TimeUnit.SECONDS);
        // The summary labels used to be populated once and never refreshed, so a
        // swap that spent SWAPS_DEPOSIT left a stale "0.001" on screen. Refresh
        // the summary on a slow cadence as well as when the tab is opened.
        GUISwap.scheduledExecutorService.scheduleAtFixedRate(
                this::refreshAllAccountBalances, 120, 120, TimeUnit.SECONDS);
        fetchFeeEstimate();

        // 99% is the default fee source. The field is filled when the Dojo
        // estimates load; if they fail and the user does not enter a rate,
        // the withdrawal is blocked instead of silently using a default.
        selectFeeButton(feeEstimate99Button);
        btcFeeTextField.focusedProperty().addListener((obs, wasFocused, isFocused) -> {
            if (isFocused) selectFeeButton(manualFeeButton);
        });

        // Periodic refresh so the selected balance and confirmation counts stay
        // current. Keep this slow: each fetch is a Tor/Dojo round trip, and the
        // all-account summary refresh was causing overlapping requests and a
        // flashing UTXO table. The table is only rebuilt when no checkboxes are
        // ticked, and successful rows are kept during a refresh.
        GUISwap.scheduledExecutorService.scheduleAtFixedRate(() -> {
            try {
                String accountName = whirlpoolAccount.getValue();
                boolean anySelected = false;
                for (UtxoRow row : utxoRows) {
                    if (row.isSelected()) {
                        anySelected = true;
                        break;
                    }
                }
                if (!anySelected) refreshUtxoTable(accountName);
            } catch (Exception e) {
                // best-effort periodic refresh; transient failures are ignored
            }
        }, 60, 60, TimeUnit.SECONDS);
    }

    /** Called each time the Withdraw tab becomes visible. */
    public void onPageShown() {
        refreshAllAccountBalances();
        String accountName = whirlpoolAccount.getValue();
        if (accountName != null) refreshUtxoTable(accountName);
    }

    private void refreshAllAccountBalances() {
        // Each account balance is an independent Dojo fetch over Tor (~1-3 s
        // each); running them sequentially made the summary bar take 10-20 s
        // to fill. Fetch all six in parallel instead, and skip a tick if the
        // previous round is still in flight (slow Tor) so fetches don't pile up.
        if (outstandingBalanceFetches.get() > 0) return;
        fetchAccountBalanceAsync(balDepositSummary, WhirlpoolAccount.DEPOSIT);
        fetchAccountBalanceAsync(balPostmixSummary, WhirlpoolAccount.POSTMIX);
        fetchAccountBalanceAsync(balBadbankSummary, WhirlpoolAccount.BADBANK);
        fetchAccountBalanceAsync(balSwapsDepositSummary, WhirlpoolAccount.SWAPS_DEPOSIT);
        fetchAccountBalanceAsync(balSwapsRefundsSummary, WhirlpoolAccount.SWAPS_REFUNDS);
        fetchAccountBalanceAsync(balSwapsAsbSummary, WhirlpoolAccount.ASB);
    }

    private void fetchAccountBalanceAsync(javafx.scene.control.Label target, WhirlpoolAccount account) {
        outstandingBalanceFetches.incrementAndGet();
        GUISwap.executorService.submit(() -> {
            try {
                swap.wallet.SwapsWallet swapsWallet = swap.wallet.SwapsWallet.getInstance();
                String value = swapsWallet == null ? "—" : fmtBalance(swapsWallet, account);
                updateGui(() -> target.setText(value));
            } catch (Exception e) {
                updateGui(() -> target.setText("—"));
            } finally {
                outstandingBalanceFetches.decrementAndGet();
            }
        });
    }

    private String fmtBalance(swap.wallet.SwapsWallet swapsWallet, WhirlpoolAccount bipWallet) {
        try {
            long sats = UnspentOutput.sumValue(swapsWallet.queryUtxos(bipWallet));
            return swap.wallet.SwapsWallet.toBtcString(Coin.valueOf(sats));
        } catch (Exception e) {
            return "—";
        }
    }

    private static void copyToClipboard(String value) {
        if (value == null || value.isBlank()) return;
        javafx.scene.input.ClipboardContent content = new javafx.scene.input.ClipboardContent();
        content.putString(value);
        javafx.scene.input.Clipboard.getSystemClipboard().setContent(content);
    }

    private void setupUtxoTable() {
        utxoTable.setItems(utxoRows);
        utxoTable.setEditable(true);
        utxoTable.setRowFactory(table -> {
            javafx.scene.control.TableRow<UtxoRow> row = new javafx.scene.control.TableRow<>();
            javafx.scene.control.MenuItem copyAddress =
                    new javafx.scene.control.MenuItem("Copy address");
            copyAddress.setOnAction(event -> {
                UtxoRow item = row.getItem();
                if (item != null) copyToClipboard(item.getAddress());
            });
            javafx.scene.control.MenuItem copyTxid =
                    new javafx.scene.control.MenuItem("Copy transaction ID");
            copyTxid.setOnAction(event -> {
                UtxoRow item = row.getItem();
                if (item != null) copyToClipboard(item.getTxid() + ":" + item.getVout());
            });
            javafx.scene.control.ContextMenu menu =
                    new javafx.scene.control.ContextMenu(copyAddress, copyTxid);
            row.contextMenuProperty().bind(
                    javafx.beans.binding.Bindings.when(row.emptyProperty())
                            .then((javafx.scene.control.ContextMenu) null)
                            .otherwise(menu));
            return row;
        });

        utxoSelectColumn.setCellValueFactory(cd -> cd.getValue().selectedProperty());
        utxoSelectColumn.setCellFactory(CheckBoxTableCell.forTableColumn(utxoSelectColumn));
        utxoSelectColumn.setEditable(true);

        utxoTxidColumn.setCellValueFactory(cd -> new ReadOnlyStringWrapper(cd.getValue().getTxidShort()));
        utxoVoutColumn.setCellValueFactory(cd -> new ReadOnlyStringWrapper(String.valueOf(cd.getValue().getVout())));
        utxoValueColumn.setCellValueFactory(cd -> new ReadOnlyStringWrapper(cd.getValue().getValueBtc()));
        utxoConfColumn.setCellValueFactory(cd -> new ReadOnlyStringWrapper(String.valueOf(cd.getValue().getConfirmations())));
        utxoAddrColumn.setCellValueFactory(cd -> new ReadOnlyStringWrapper(cd.getValue().getAddressShort()));
    }

    // Dojo next-block fee estimator (probability → sat/vB), fetched on demand
    // and cached; keys "0.99"/"0.5"/"0.1" back the 99%/50%/10% buttons.
    private volatile Map<String, Integer> feeEstimates = null;

    // Auto-populate the fee field from the paired Dojo's next-block fee
    // estimator (99% confidence target). Manual entry always wins: a
    // non-empty field is never overwritten. If the fetch fails, the field
    // stays empty and withdrawal is blocked until the user enters a rate.
    private void fetchFeeEstimate() {
        GUISwap.executorService.submit(() -> {
            try {
                swap.wallet.SwapsWallet swapsWallet = swap.wallet.SwapsWallet.getInstance();
                if (swapsWallet == null) return;
                Map<String, Integer> estimates = swapsWallet.fetchFeeEstimates();
                if (estimates == null || estimates.isEmpty()) return;
                feeEstimates = estimates;
                Platform.runLater(WithdrawController.this::updateFeeButtonLabels);
                Integer satPerVb = estimates.get("0.99");
                if (satPerVb == null) return;
                Platform.runLater(() -> {
                    String current = btcFeeTextField.getText();
                    if (current == null || current.isEmpty()) {
                        btcFeeTextField.setText(String.valueOf(satPerVb));
                        selectFeeButton(feeEstimate99Button);
                    }
                });
            } catch (Exception e) {
                Platform.runLater(() ->
                        MainController.getInstance().printSwapLogLn(LogType.WARN,
                                "Dojo fee estimate unavailable; keeping manual fee entry.", false));
            }
        });
    }

    // Stamps the live next-block rates onto the estimate buttons, stacked
    // under their confidence: "99%" / "2 sat". Until estimates load the
    // second line shows a dash. Must run on the FX thread.
    private void updateFeeButtonLabels() {
        Map<String, Integer> estimates = feeEstimates;
        if (estimates == null) return;
        stampFeeLabel(feeEstimate99Button, estimates.get("0.99"));
        stampFeeLabel(feeEstimate50Button, estimates.get("0.5"));
        stampFeeLabel(feeEstimate10Button, estimates.get("0.1"));
    }

    private void stampFeeLabel(Button button, Integer satPerVb) {
        if (button == null) return;
        String confidence = button.getText().split("\n")[0];
        button.setText(confidence + "\n" + (satPerVb == null ? "– sat" : satPerVb + " sat"));
    }

    // Highlights the active fee source: the selected button gets the oxblood
    // accent fill, the rest render plain. Must run on the FX thread. The font
    // weight stays the same: switching to bold made the text wider than the
    // space JavaFX had measured for it, and the "%" of "99%" was clipped.
    private void selectFeeButton(Button button) {
        selectedFeeButton = button;
        for (Button b : new Button[]{feeEstimate99Button, feeEstimate50Button, feeEstimate10Button, manualFeeButton}) {
            if (b != null) b.setStyle(b == selectedFeeButton
                    ? "-fx-background-color: #8f1a14; -fx-text-fill: white;"
                    : "");
        }
    }

    // Clicking a confidence button (99%/50%/10%) fills the fee field with
    // that target's next-block rate, fetching estimates if not cached yet.
    @FXML
    public void onFeeEstimateClick(javafx.event.ActionEvent event) {
        String target;
        Object source = event.getSource();
        if (source == feeEstimate99Button) target = "0.99";
        else if (source == feeEstimate50Button) target = "0.5";
        else target = "0.1";
        selectFeeButton((Button) source);

        GUISwap.executorService.submit(() -> {
            try {
                if (feeEstimates == null) {
                    swap.wallet.SwapsWallet swapsWallet = swap.wallet.SwapsWallet.getInstance();
                    if (swapsWallet == null) return;
                    Map<String, Integer> estimates = swapsWallet.fetchFeeEstimates();
                    if (estimates == null || estimates.isEmpty()) return;
                    feeEstimates = estimates;
                }
                Integer satPerVb = feeEstimates.get(target);
                if (satPerVb == null) return;
                Platform.runLater(() -> {
                    updateFeeButtonLabels();
                    btcFeeTextField.setText(String.valueOf(satPerVb));
                    selectFeeButton((Button) source);
                });
            } catch (Exception e) {
                Platform.runLater(() ->
                        MainController.getInstance().printSwapLogLn(LogType.WARN,
                                "Dojo fee estimate unavailable; enter the fee manually.", false));
            }
        });
    }

    // Manual mode: mark the manual button selected and hand focus to the
    // field — no estimate fetch, the user types the rate.
    @FXML
    public void onManualFeeClick(javafx.event.ActionEvent event) {
        selectFeeButton(manualFeeButton);
        btcFeeTextField.requestFocus();
    }

    private void refreshUtxoTable(String accountName) {
        if (accountName == null) return;
        WhirlpoolAccount bipWallet = wallets.get(accountName);
        if (bipWallet == null) return;
        long myGeneration = utxoRefreshGeneration.incrementAndGet();
        GUISwap.executorService.submit(() -> {
            swap.wallet.SwapsWallet swapsWallet = swap.wallet.SwapsWallet.getInstance();
            if (swapsWallet == null) {
                if (myGeneration == utxoRefreshGeneration.get()) {
                    updateGui(() -> utxoSelectionSummary.setText("wallet not initialized"));
                }
                return;
            }
            try {
                Collection<UnspentOutput> utxos = swapsWallet.queryUtxos(bipWallet);
                List<UtxoRow> rows = new ArrayList<>();
                for (UnspentOutput utxo : utxos) {
                    UtxoRow row = new UtxoRow(utxo);
                    row.selectedProperty().addListener((obs, oldV, newV) -> updateSelectionSummary());
                    rows.add(row);
                }
                // Discard if a newer refresh (from switching accounts again) has since
                // started - otherwise a slow, stale fetch could overwrite the table with
                // the wrong account's UTXOs after the user has already moved on.
                if (myGeneration != utxoRefreshGeneration.get()) return;
                long sats = UnspentOutput.sumValue(utxos);
                int count = utxos.size();
                updateGui(() -> {
                    utxoRows.setAll(rows);
                    updateSelectionSummary();
                    selectedAccountBalance.setText("Balance: "
                            + Coin.valueOf(sats).toPlainString() + " BTC  (" + count
                            + (count == 1 ? " UTXO)" : " UTXOs)"));
                });
            } catch (Exception e) {
                if (myGeneration != utxoRefreshGeneration.get()) return;
                updateGui(() -> {
                    utxoSelectionSummary.setText("error: " + e.getMessage());
                    if (utxoRows.isEmpty()) selectedAccountBalance.setText("Balance: —");
                });
            }
        });
    }

    private void updateSelectionSummary() {
        long sumSats = 0;
        int count = 0;
        for (UtxoRow row : utxoRows) {
            if (row.isSelected()) {
                sumSats += row.getValueSats();
                count++;
            }
        }
        utxoSelectionSummary.setText(count + " selected, " + Coin.valueOf(sumSats).toPlainString() + " BTC");
        if (sendAllMode) updateSendAllText();
        else sendAllButton.setText(sendAllIdleLabel());
    }

    /** "Send All" sweeps the account; with UTXOs ticked it sweeps only those, and the label says so. */
    private String sendAllIdleLabel() {
        int count = 0;
        for (UtxoRow row : utxoRows) {
            if (row.isSelected()) count++;
        }
        return count > 0 ? "Send Selected (" + count + ")" : "Send All";
    }

    /** Reflects the sweep scope in the (disabled) amount field while Send All is on. */
    private void updateSendAllText() {
        int count = 0;
        for (UtxoRow row : utxoRows) {
            if (row.isSelected()) count++;
        }
        btcAmountTextField.setText(count > 0
                ? "MAX — sweep of " + count + " selected UTXO" + (count == 1 ? "" : "s")
                : "MAX — sweep of full account balance");
    }

    @FXML
    public void onSendAllButtonClick(ActionEvent actionEvent) {
        sendAllMode = !sendAllMode;
        if (sendAllMode) {
            btcAmountTextField.setDisable(true);
            updateSendAllText();
            sendAllButton.setText("Use Amount");
        } else {
            btcAmountTextField.setDisable(false);
            btcAmountTextField.clear();
            sendAllButton.setText(sendAllIdleLabel());
        }
    }

    @FXML
    public void onUtxoSelectAllClick(ActionEvent e) {
        for (UtxoRow row : utxoRows) row.selectedProperty().set(true);
        updateSelectionSummary();
    }

    @FXML
    public void onUtxoClearClick(ActionEvent e) {
        for (UtxoRow row : utxoRows) row.selectedProperty().set(false);
        updateSelectionSummary();
    }

    @FXML
    public void onWithdrawButtonClick(ActionEvent actionEvent) {
        GUISwap.executorService.submit(() -> {
            try {
                String addressString = btcAddressTextField.getText();
                Address address = HelperAddress.getAddress(addressString);
                if (address == null) {
                    MainController.getInstance().printSwapLogLn(LogType.WARN, "Invalid Bitcoin address.", false);
                    return;
                }
                // Send-all mode bypasses the amount field entirely: prepareWithdraw
                // sweeps (Long.MAX_VALUE sentinel), so the exact amount is
                // sum(selected UTXOs) - fee, shown in the confirmation popup.
                Coin amount;
                if (sendAllMode) {
                    amount = Coin.valueOf(Long.MAX_VALUE);
                } else {
                    String amountString = btcAmountTextField.getText();
                    if (amountString == null || amountString.isEmpty()) {
                        MainController.getInstance().printSwapLogLn(LogType.WARN, "Amount is empty.", false);
                        return;
                    }
                    amount = Coin.parseCoin(amountString);
                }
                String feeRateString = btcFeeTextField.getText();
                if (feeRateString == null || feeRateString.trim().isEmpty()) {
                    MainController.getInstance().printSwapLogLn(LogType.WARN,
                            "Select a fee estimate or enter a fee rate before withdrawing.", false);
                    return;
                }
                long feeRate;
                try {
                    feeRate = Long.parseLong(feeRateString.trim());
                } catch (NumberFormatException e) {
                    MainController.getInstance().printSwapLogLn(LogType.WARN,
                            "Fee rate must be a whole number of sat/vB.", false);
                    return;
                }
                if (feeRate <= 0) {
                    MainController.getInstance().printSwapLogLn(LogType.WARN,
                            "Fee rate must be greater than 0 sat/vB.", false);
                    return;
                }

                WhirlpoolAccount sourceWallet = wallets.get(whirlpoolAccount.getValue());
                if (sourceWallet == null) {
                    MainController.getInstance().printSwapLogLn(LogType.WARN, "Source account not selected.", false);
                    return;
                }

                swap.wallet.SwapsWallet swapsWallet = swap.wallet.SwapsWallet.getInstance();
                if (swapsWallet == null) {
                    MainController.getInstance().printSwapLogLn(LogType.ERROR, "Wallet not initialized.", false);
                    return;
                }

                // Collect any user-selected UTXOs. Empty list → auto-select (linear).
                List<UnspentOutput> selectedUtxos = new ArrayList<>();
                for (UtxoRow row : utxoRows) {
                    if (row.isSelected()) selectedUtxos.add(row.getUtxo());
                }

                String selectionDescription = selectedUtxos.isEmpty()
                        ? "auto-select"
                        : selectedUtxos.size() + " explicit UTXO" + (selectedUtxos.size() == 1 ? "" : "s");
                if (sendAllMode) {
                    MainController.getInstance().printSwapLogLn(LogType.INFO,
                            "Preparing SEND-ALL sweep from " + sourceWallet + " at " + feeRate
                                    + " sat/vB (" + selectionDescription + ")...", false);
                } else {
                    MainController.getInstance().printSwapLogLn(LogType.INFO,
                            "Preparing withdrawal: " + swap.wallet.SwapsWallet.toBtcString(amount) + " BTC from " + sourceWallet
                                    + " at " + feeRate + " sat/vB (" + selectionDescription + ")...", false);
                }

                swap.wallet.SwapsWallet.WithdrawPlan plan =
                        swapsWallet.prepareWithdraw(address, amount, sourceWallet, feeRate,
                                selectedUtxos.isEmpty() ? null : selectedUtxos);

                Platform.runLater(() ->
                    swap.gui.controller.popups.PopupConfirmWithdraw.create(plan, () ->
                        GUISwap.executorService.submit(() -> {
                            try {
                                String response = swapsWallet.broadcastWithdraw(plan);
                                MainController.getInstance().printSwapLogLn(LogType.HIGHLIGHT,
                                        "Withdrawal broadcast: " + response, false);
                                // Refresh balance + UTXO list after broadcast so the spent UTXOs disappear.
                                refreshUtxoTable(whirlpoolAccount.getValue());
                                refreshAllAccountBalances();
                                swapsWallet.refreshAsbBalanceNow();
                                // A completed sweep has nothing left to sweep; drop back
                                // to the normal amount field for the next withdrawal.
                                if (sendAllMode) {
                                    Platform.runLater(() -> {
                                        sendAllMode = false;
                                        btcAmountTextField.setDisable(false);
                                        btcAmountTextField.clear();
                                        sendAllButton.setText(sendAllIdleLabel());
                                    });
                                }
                            } catch (Exception e) {
                                MainController.getInstance().printSwapLogLn(LogType.ERROR,
                                        "Broadcast failed: " + e.getMessage(), false);
                                e.printStackTrace();
                            }
                        })
                    )
                );
            } catch (Exception e) {
                MainController.getInstance().printSwapLogLn(LogType.ERROR, "Withdrawal failed: " + e.getMessage(), false);
                e.printStackTrace();
            }
        });
    }

}
