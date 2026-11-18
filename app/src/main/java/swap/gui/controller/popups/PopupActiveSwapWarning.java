package swap.gui.controller.popups;

import javafx.application.Platform;
import javafx.fxml.FXML;
import javafx.fxml.FXMLLoader;
import javafx.scene.Parent;
import javafx.scene.Scene;
import javafx.scene.control.Button;
import javafx.scene.control.Label;
import javafx.scene.paint.Color;
import javafx.stage.Modality;
import javafx.stage.Stage;
import swap.gui.GUISwap;

/**
 * Themed confirmation shown when the window is closed, or the swap is disconnected, while a
 * swap is still running. Returns true only when the user explicitly chooses the dangerous
 * button; the window close button, Escape and the safe button all return false.
 */
public class PopupActiveSwapWarning {

    @FXML private Label summaryText;
    @FXML private Label asbNote;
    @FXML private Button keepOpenButton;
    @FXML private Button closeAnywayButton;
    @FXML private Label headerText;
    @FXML private Label warningTitle;
    @FXML private Label warningBody;

    private boolean closeAnyway = false;

    public static boolean confirm(Stage owner, String summary, boolean asbRunning) {
        return show(owner, summary, asbRunning, null);
    }

    /** Wording for the swap panel's "Disconnect" button (audit M-5). */
    static final String[] DISCONNECT_TEXT = {
            "Keep the swap connected until it finishes.",
            "DISCONNECTING CAN LOSE BTC",
            "Disconnecting stops swap monitoring. Your Bitcoin may be locked: if the swap needs a refund, missing the punish window can lose your BTC. You can resume the swap from History.",
            "Keep swap running",
            "Disconnect anyway",
    };

    public static boolean confirmDisconnect(Stage owner, String summary) {
        return show(owner, summary, false, DISCONNECT_TEXT);
    }

    /**
     * A seller asked a swap fee rate above our limit (SellerFeeAllowance, matrix S-01). The safe
     * choice, "Try another seller", is the default; true means the user accepted the rate.
     */
    public static boolean confirmSellerFee(Stage owner, long sellerRate, long ourMax, long ourEstimate) {
        return show(owner, swap.model.SellerFeeAllowance.summary(sellerRate, ourMax, ourEstimate), false, new String[]{
                swap.model.SellerFeeAllowance.title(),
                swap.model.SellerFeeAllowance.costTitle(),
                swap.model.SellerFeeAllowance.costNote(sellerRate, ourEstimate),
                swap.model.SellerFeeAllowance.declineLabel(),
                swap.model.SellerFeeAllowance.acceptLabel(sellerRate)
        }, "Seller fee rate");
    }

    private static boolean show(Stage owner, String summary, boolean asbRunning, String[] text) {
        return show(owner, summary, asbRunning, text, "Swap in progress");
    }

    private static boolean show(Stage owner, String summary, boolean asbRunning, String[] text, String windowTitle) {
        try {
            FXMLLoader loader = new FXMLLoader(GUISwap.class.getResource("/popups/popup-active-swap-warning.fxml"));
            Parent root = loader.load();

            Stage stage = new Stage();
            stage.initOwner(owner);
            stage.initModality(Modality.WINDOW_MODAL);
            stage.setTitle(windowTitle);

            Scene scene = new Scene(root, 640, 400, Color.web("#0e0e0e"));
            GUISwap.applyTheme(scene);
            stage.setScene(scene);

            PopupActiveSwapWarning controller = loader.getController();
            controller.summaryText.setText(summary);
            controller.asbNote.setManaged(asbRunning);
            controller.asbNote.setVisible(asbRunning);
            if (text != null) {
                controller.headerText.setText(text[0]);
                controller.warningTitle.setText(text[1]);
                controller.warningBody.setText(text[2]);
                controller.keepOpenButton.setText(text[3]);
                controller.closeAnywayButton.setText(text[4]);
            }

            stage.setAlwaysOnTop(true);
            Platform.runLater(controller.keepOpenButton::requestFocus);
            stage.showAndWait();
            return controller.closeAnyway;
        } catch (Exception e) {
            e.printStackTrace();
            return false;
        }
    }

    @FXML
    private void onKeepOpen() {
        close();
    }

    @FXML
    private void onCloseAnyway() {
        closeAnyway = true;
        close();
    }

    private void close() {
        ((Stage) keepOpenButton.getScene().getWindow()).close();
    }
}
