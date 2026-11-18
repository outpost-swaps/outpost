package swap.gui.controller.popups;

import com.samourai.wallet.api.backend.beans.UnspentOutput;
import javafx.fxml.FXML;
import javafx.fxml.FXMLLoader;
import javafx.scene.Parent;
import javafx.scene.Scene;
import javafx.scene.control.Button;
import javafx.scene.control.Label;
import javafx.scene.control.TextArea;
import javafx.scene.control.TitledPane;
import javafx.scene.control.Tooltip;
import javafx.scene.layout.VBox;
import javafx.scene.paint.Color;
import javafx.stage.Stage;
import org.bitcoinj.core.Coin;
import swap.gui.GUISwap;
import swap.wallet.SwapsWallet.WithdrawPlan;

public class PopupConfirmWithdraw {

    @FXML private Label amountHeadline;
    @FXML private Label destinationHeadline;
    @FXML private Label fromValue;
    @FXML private Label feeValue;
    @FXML private Label totalValue;
    @FXML private Label rateValue;
    @FXML private TitledPane detailsPane;
    @FXML private VBox warningBox;
    @FXML private TextArea txStructure;
    @FXML private Label warningText;
    @FXML private Button confirmButton;
    @FXML private Button cancelButton;

    private WithdrawPlan plan;
    private Runnable onConfirmCallback;

    /**
     * Show withdrawal confirm dialog with the prepared tx structure.
     * onConfirm fires only if user clicks Broadcast Anyway.
     */
    public static void create(WithdrawPlan plan, Runnable onConfirm) {
        try {
            FXMLLoader loader = new FXMLLoader(GUISwap.class.getResource("/popups/popup-confirm-withdraw.fxml"));
            Parent root = loader.load();
            Stage stage = new Stage();
            stage.setTitle("Confirm Withdrawal");
            Scene scene = new Scene(root, 760, 700, Color.web("#0e0e0e"));
            GUISwap.applyTheme(scene);
            stage.setScene(scene);
            PopupConfirmWithdraw controller = loader.getController();
            controller.plan = plan;
            controller.onConfirmCallback = onConfirm;
            controller.initialize();
            stage.setAlwaysOnTop(true);
            stage.show();
            stage.requestFocus();
            stage.toFront();
        } catch (Exception e) {
            e.printStackTrace();
        }
    }

    @FXML
    private void onConfirm() {
        Stage stage = (Stage) confirmButton.getScene().getWindow();
        stage.close();
        if (onConfirmCallback != null) onConfirmCallback.run();
    }

    @FXML
    private void onCancel() {
        Stage stage = (Stage) cancelButton.getScene().getWindow();
        stage.close();
    }

    private void initialize() {
        long feeRate = plan.feeRate;
        int vbytes = plan.vbytes();
        long totalSats = plan.amount.getValue() + plan.fee.getValue();

        amountHeadline.setText(plan.amount.toPlainString() + " BTC");
        destinationHeadline.setText("to " + plan.destination);
        destinationHeadline.setTooltip(new Tooltip(plan.destination.toString()));

        fromValue.setText(accountLabel(plan.sourceWallet));
        feeValue.setText(plan.fee.toPlainString() + " BTC");
        totalValue.setText(Coin.valueOf(totalSats).toPlainString() + " BTC");
        rateValue.setText(feeRate + " sat/vB (" + vbytes + " vB)");
        detailsPane.setText(String.format("Transaction details  (%d input%s → %s output%s)",
                plan.inputs.size(), plan.inputs.size() == 1 ? "" : "s",
                plan.changeAddress != null ? "2" : "1",
                plan.changeAddress != null ? "s" : ""));

        // One address column so amounts start at the same offset on every row.
        int addrWidth = plan.destination.toString().length();
        if (plan.changeAddress != null) {
            addrWidth = Math.max(addrWidth, plan.changeAddress.toString().length());
        }
        for (UnspentOutput utxo : plan.inputs) {
            if (utxo.addr != null) addrWidth = Math.max(addrWidth, utxo.addr.length());
        }
        addrWidth += 2;

        StringBuilder sb = new StringBuilder();
        sb.append(String.format("inputs  %d%n", plan.inputs.size()));
        long sumIn = 0;
        for (UnspentOutput utxo : plan.inputs) {
            sumIn += utxo.value;
            sb.append("  ")
              .append(pad(utxo.addr != null ? utxo.addr : "?", addrWidth))
              .append(pad(Coin.valueOf(utxo.value).toPlainString() + " BTC", 16))
              .append(shorten(utxo.tx_hash)).append(':').append(utxo.tx_output_n)
              .append('\n');
        }
        // Separate the UTXO list from its totals, and the totals from the outputs.
        sb.append('\n');
        sb.append("  ").append(pad("in", addrWidth))
          .append(Coin.valueOf(sumIn).toPlainString()).append(" BTC\n");
        sb.append("  ").append(pad("fee", addrWidth)).append(pad(plan.fee.toPlainString() + " BTC", 16))
          .append(feeRate).append(" sat/vB (").append(vbytes).append(" vB)\n");
        sb.append("  ").append(pad("total", addrWidth))
          .append(Coin.valueOf(totalSats).toPlainString()).append(" BTC\n\n");

        sb.append(String.format("outputs %d%n", plan.changeAddress != null ? 2 : 1));
        sb.append("  ").append(pad(plan.destination.toString(), addrWidth))
          .append(pad(plan.amount.toPlainString() + " BTC", 16)).append("destination\n");
        if (plan.changeAddress != null) {
            sb.append("  ").append(pad(plan.changeAddress.toString(), addrWidth))
              .append(pad(plan.change.toPlainString() + " BTC", 16)).append("change\n");
        }
        txStructure.setText(sb.toString());

        if (plan.inputs.size() > 1) {
            warningBox.setVisible(true);
            warningBox.setManaged(true);
            warningText.setText(String.format(
                    "Merging %d UTXOs links them to one wallet on every block explorer. " +
                    "Sweep one at a time to keep them unlinked.",
                    plan.inputs.size()));
        } else {
            warningBox.setVisible(false);
            warningBox.setManaged(false);
        }
    }

    private static String accountLabel(com.samourai.whirlpool.client.wallet.beans.WhirlpoolAccount account) {
        return switch (account.name()) {
            case "ASB" -> "Swaps ASB account";
            case "SWAPS_DEPOSIT" -> "Swaps deposit account";
            case "SWAPS_REFUNDS" -> "Swaps refunds account";
            default -> account.name().replace('_', ' ').toLowerCase() + " account";
        };
    }

    private static String pad(String value, int width) {
        if (value.length() >= width) return value + " ";
        return value + " ".repeat(width - value.length());
    }

    /** Middle-elided form: first 10 and last 6 characters. */
    private static String shorten(String value) {
        if (value == null) return "?";
        if (value.length() <= 20) return value;
        return value.substring(0, 10) + "…" + value.substring(value.length() - 6);
    }
}
