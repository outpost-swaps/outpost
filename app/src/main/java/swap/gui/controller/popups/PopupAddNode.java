package swap.gui.controller.popups;

import javafx.event.ActionEvent;
import javafx.fxml.FXML;
import javafx.fxml.FXMLLoader;
import javafx.scene.Parent;
import javafx.scene.Scene;
import javafx.scene.control.Button;
import javafx.scene.control.CheckBox;
import javafx.scene.control.Label;
import javafx.scene.control.TextField;
import javafx.scene.layout.VBox;
import javafx.scene.paint.Color;
import javafx.stage.Stage;
import swap.gui.GUISwap;
import swap.helper.HelperOwnNodes;

public class PopupAddNode {

    @FXML
    private Label titleLabel;
    @FXML
    private Label subtitleLabel;
    @FXML
    private Label ownHint;
    @FXML
    private VBox portBox;
    @FXML
    private Button saveButton;
    @FXML
    private Label portFieldLabel;
    @FXML
    private TextField portText;
    @FXML
    private Button cancelButton;
    @FXML
    private TextField urlText;
    @FXML
    private CheckBox ownNodeCheck;

    private boolean askOwn = false;
    private boolean ownTouched = false;

    private OwnListener listener;
    private String hint = "";
    private String title = "";
    private boolean showPortField = true;

    public static PopupAddNode create(String title, String hint, boolean showPortField, Listener listener) {
        return create(title, hint, showPortField, false, false, (address, port, own) -> listener.onSaveNode(address, port));
    }

    /**
     * @param askOwn     show "This is a personal node"
     * @param ownPreset  start it ticked and leave it ticked (the user asked for "Personal node")
     */
    public static PopupAddNode create(String title, String hint, boolean showPortField, boolean askOwn, boolean ownPreset, OwnListener listener) {
        try {
            FXMLLoader fxmlLoader = new FXMLLoader(GUISwap.class.getResource("/popups/popup-add-node.fxml"));
            Parent root = fxmlLoader.load();
            Stage stage = new Stage();
            stage.setTitle(title);
            Scene scene = new Scene(root, Color.web("#0e0e0e"));
            GUISwap.applyTheme(scene);
            stage.setScene(scene);
            stage.setResizable(false);
            PopupAddNode popupAddNode = fxmlLoader.getController();
            popupAddNode.listener = listener;
            popupAddNode.hint = hint;
            popupAddNode.title = title;
            popupAddNode.showPortField = showPortField;
            popupAddNode.askOwn = askOwn;
            popupAddNode.initialize();
            if (askOwn && ownPreset) {
                popupAddNode.ownNodeCheck.setSelected(true);
                popupAddNode.ownTouched = true;
            }
            stage.show();
            return popupAddNode;
        } catch (Exception e) {
            e.printStackTrace();
            return null;
        }
    }


    @FXML
    private void save(ActionEvent actionEvent) {
        String url = urlText.getText();
        String port = portText.getText();
        if(showPortField) {
            if(url == null || port == null) return;
            if (!url.isEmpty() && !port.isEmpty()) {
                try {
                    this.listener.onSaveNode(url.trim(), Integer.parseInt(port.trim()), ownNodeCheck.isSelected());
                    closeWindow();
                } catch (Exception e) {
                    e.printStackTrace();
                }
            }
        } else {
            if(url == null) return;
            if (!url.isEmpty()) {
                try {
                    this.listener.onSaveNode(url.trim(), -1, false);
                    closeWindow();
                } catch (Exception e) {
                    e.printStackTrace();
                }
            }
        }

    }

    @FXML
    private void closeWindow() {
        Stage stage = (Stage) cancelButton.getScene().getWindow();
        stage.close();
    }

    /** The explanation under "I run this ... myself": what ticking it changes, and when not to. */
    static String ownHintText(String noun) {
        if ("node".equals(noun)) {
            return "Only for a node you run yourself: Outpost then trusts it alone to confirm a seller's Monero lock. "
                    + "Local addresses are ticked for you.";
        }
        return "Only for a server you run yourself: Outpost then uses it alone, never a public fallback. "
                + "Local addresses are ticked for you.";
    }

    private void initialize() {
        titleLabel.setText(title);
        subtitleLabel.setText(subtitle());
        ownNodeCheck.setText("Personal " + noun());
        ownHint.setText(ownHintText(noun()));
        urlText.setPromptText(hint);
        portBox.setVisible(showPortField);
        portBox.setManaged(showPortField);
        ownHint.setVisible(askOwn);
        ownHint.setManaged(askOwn);
        saveButton.disableProperty().bind(javafx.beans.binding.Bindings.createBooleanBinding(
                () -> !inputValid(urlText.getText(), portText.getText(), showPortField),
                urlText.textProperty(), portText.textProperty()));
        ownNodeCheck.setVisible(askOwn);
        ownNodeCheck.setManaged(askOwn);
        // Until the user touches the box, it follows what they type: only local addresses are
        // ticked for them (an onion is usually someone else's node).
        urlText.textProperty().addListener((obs, oldVal, newVal) -> {
            if (!ownTouched) {
                ownNodeCheck.setSelected(HelperOwnNodes.suggestOwn(newVal));
            }
        });
        ownNodeCheck.setOnAction(e -> ownTouched = true);
    }

    /** Electrum has servers; Monero has nodes. */
    private String noun() {
        return title.toLowerCase().contains("electrum") ? "server" : "node";
    }

    private String subtitle() {
        return showPortField
                ? "Enter the " + noun() + "'s address and port."
                : "Enter the peer's full address.";
    }

    /** Save is allowed once there is an address and, where a port is asked for, a valid one. */
    static boolean inputValid(String url, String port, boolean needPort) {
        if (url == null || url.isBlank()) {
            return false;
        }
        if (!needPort) {
            return true;
        }
        try {
            int p = Integer.parseInt(port == null ? "" : port.trim());
            return p > 0 && p <= 65535;
        } catch (NumberFormatException e) {
            return false;
        }
    }

    public interface Listener {
        void onSaveNode(String address, int port);
    }

    /** Like {@link Listener}, also told whether "This is a personal node" was ticked. */
    public interface OwnListener {
        void onSaveNode(String address, int port, boolean own);
    }
}
