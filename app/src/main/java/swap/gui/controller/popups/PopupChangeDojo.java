package swap.gui.controller.popups;

import java.io.File;
import java.io.IOException;
import java.util.Optional;
import javafx.application.Platform;
import javafx.fxml.FXML;
import javafx.fxml.FXMLLoader;
import javafx.scene.Node;
import javafx.scene.Parent;
import javafx.scene.Scene;
import javafx.scene.control.Button;
import javafx.scene.control.ComboBox;
import javafx.scene.control.Label;
import javafx.scene.control.PasswordField;
import javafx.scene.control.RadioButton;
import javafx.scene.control.TextArea;
import javafx.scene.control.TextField;
import javafx.scene.control.ToggleGroup;
import javafx.scene.layout.VBox;
import javafx.scene.paint.Color;
import javafx.stage.Modality;
import javafx.stage.Stage;
import javafx.stage.Window;
import javafx.util.StringConverter;
import swap.gui.GUISwap;
import swap.helper.HelperDojoDirectory;
import swap.helper.HelperDojoDirectoryLoader;
import swap.helper.HelperDojoListStatus;
import swap.helper.HelperDojoLogin;
import swap.helper.HelperNetwork;
import swap.helper.HelperProperties;
import swap.helper.HelperTorPort;
import swap.helper.HelperWallet;
import swap.helper.HelperWalletImport;

/**
 * Settings &rarr; Wallet &rarr; Dojo: switch to another Dojo after the wallet is connected, from the
 * verified public list, the user's own (JSON or URL and key) or an Ashigaru pairing payload (only its
 * Dojo is taken). A Dojo can only be saved after a test login over Tor succeeded, so a wrong key
 * never replaces a working Dojo, and saving needs the wallet passphrase (the key is stored encrypted
 * with it). The new Dojo is used after the next start, so the whole app switches together.
 */
public class PopupChangeDojo {
    @FXML private Label currentDojo;
    @FXML private ToggleGroup source;
    @FXML private VBox publicCard;
    @FXML private VBox ownCard;
    @FXML private VBox payloadCard;
    @FXML private RadioButton publicMode;
    @FXML private RadioButton ownMode;
    @FXML private RadioButton payloadMode;
    @FXML private VBox publicInputs;
    @FXML private VBox ownInputs;
    @FXML private VBox payloadInputs;
    @FXML private ComboBox<HelperDojoDirectory.Entry> dojoChoice;
    @FXML private Label listStatus;
    @FXML private TextArea dojoJson;
    @FXML private TextField dojoUrl;
    @FXML private PasswordField dojoKey;
    @FXML private TextArea payloadText;
    @FXML private PasswordField payloadPass;
    @FXML private Button testButton;
    @FXML private Label testResult;
    @FXML private PasswordField walletPassphrase;
    @FXML private Label messageLabel;
    @FXML private Button saveButton;

    private Stage stage;
    private String savedUrl;
    private HelperWalletImport.Dojo chosen;
    private String testedFor;
    private boolean testing;

    private HelperDojoDirectory.Directory directory = HelperDojoDirectory.bundled();
    private HelperDojoDirectoryLoader.Source listSource = HelperDojoDirectoryLoader.Source.SHIPPED;
    private boolean updatingList;

    /** Shows the window and waits; the new Dojo's URL if one was saved, otherwise empty. */
    public static Optional<String> show(Window owner) {
        try {
            FXMLLoader loader = new FXMLLoader(GUISwap.class.getResource("/popups/popup-change-dojo.fxml"));
            Parent root = loader.load();
            Stage stage = new Stage();
            if (owner != null) {
                stage.initOwner(owner);
            }
            stage.initModality(Modality.WINDOW_MODAL);
            stage.setTitle("Change Dojo");
            Scene scene = new Scene(root, 660, 780, Color.web("#0e0e0e"));
            GUISwap.applyTheme(scene);
            stage.setScene(scene);

            PopupChangeDojo controller = loader.getController();
            controller.stage = stage;
            controller.start();
            stage.showAndWait();
            controller.clearSecrets();
            return Optional.ofNullable(controller.savedUrl);
        } catch (Exception e) {
            e.printStackTrace();
            return Optional.empty();
        }
    }

    private void start() {
        String current = HelperProperties.dojoUrl;
        currentDojo.setText(current == null || current.isBlank() ? "none" : current);

        source.selectedToggleProperty().addListener((obs, oldVal, newVal) -> {
            syncCards();
            refresh();
        });
        for (TextField field : new TextField[] {dojoUrl, dojoKey, payloadPass, walletPassphrase}) {
            field.textProperty().addListener((obs, oldVal, newVal) -> refresh());
        }
        dojoJson.textProperty().addListener((obs, oldVal, newVal) -> refresh());
        payloadText.textProperty().addListener((obs, oldVal, newVal) -> refresh());
        dojoChoice.setConverter(new StringConverter<>() {
            @Override
            public String toString(HelperDojoDirectory.Entry entry) {
                return entry == null ? "" : entry.label();
            }

            @Override
            public HelperDojoDirectory.Entry fromString(String text) {
                return null;
            }
        });
        dojoChoice.valueProperty().addListener((obs, oldVal, newVal) -> {
            if (!updatingList) {
                refresh();
            }
        });
        syncCards();
        loadDojoList();
        refresh();
    }

    // ---- the public list -----------------------------------------------------------------------

    private boolean testnet() {
        try {
            return HelperNetwork.isTestnet(GUISwap.appSwap.getParams());
        } catch (Throwable e) {
            return false;
        }
    }

    private void fillList() {
        String chosenId = dojoChoice.getValue() == null ? null : dojoChoice.getValue().id();
        updatingList = true;
        try {
            dojoChoice.getItems().setAll(directory.forNetwork(testnet()));
            dojoChoice.setValue(null);
            if (chosenId != null) {
                for (HelperDojoDirectory.Entry entry : dojoChoice.getItems()) {
                    if (entry.id().equals(chosenId)) {
                        dojoChoice.setValue(entry);
                    }
                }
            }
        } finally {
            updatingList = false;
        }
    }

    private void loadDojoList() {
        File cache;
        try {
            cache = new File(swap.lib.AppSwap.getSwapRootDir(), "dojo-directory-cache.json");
        } catch (Throwable e) {
            cache = null;
        }
        final File cacheFile = cache;
        HelperDojoDirectoryLoader.Loaded offline = HelperDojoDirectoryLoader.loadOffline(cacheFile);
        directory = offline.directory();
        listSource = offline.source();
        fillList();
        listStatus.setText(HelperDojoListStatus.text(listSource, directory, HelperDojoListStatus.Phase.LOADING, null));

        Thread fetch = new Thread(() -> {
            Integer port = HelperTorPort.waitForSocksPort(60_000);
            if (port == null) {
                later(() -> listStatus.setText(HelperDojoListStatus.text(listSource, directory, HelperDojoListStatus.Phase.FAILED, "Tor is not ready")));
                return;
            }
            try {
                HelperDojoDirectoryLoader.Loaded live = HelperDojoDirectoryLoader.loadLive(port, cacheFile);
                later(() -> {
                    directory = live.directory();
                    listSource = live.source();
                    fillList();
                    listStatus.setText(HelperDojoListStatus.text(listSource, directory, HelperDojoListStatus.Phase.DONE, null));
                });
            } catch (Exception e) {
                String reason = HelperDojoDirectoryLoader.reason(e);
                later(() -> listStatus.setText(HelperDojoListStatus.text(listSource, directory, HelperDojoListStatus.Phase.FAILED, reason)));
            }
        }, "change-dojo-directory");
        fetch.setDaemon(true);
        fetch.start();
    }

    /** Runs on the UI thread, and only while the window is still open. */
    private void later(Runnable action) {
        Platform.runLater(() -> {
            if (stage != null && stage.isShowing()) {
                action.run();
            }
        });
    }

    // ---- choosing and testing ------------------------------------------------------------------

    private static void show(Node node, boolean visible) {
        node.setVisible(visible);
        node.setManaged(visible);
    }

    private static void selected(VBox card, boolean on) {
        card.getStyleClass().remove("option-card-selected");
        if (on) {
            card.getStyleClass().add("option-card-selected");
        }
    }

    private void syncCards() {
        show(publicInputs, publicMode.isSelected());
        show(ownInputs, ownMode.isSelected());
        show(payloadInputs, payloadMode.isSelected());
        selected(publicCard, publicMode.isSelected());
        selected(ownCard, ownMode.isSelected());
        selected(payloadCard, payloadMode.isSelected());
    }

    private void refresh() {
        HelperWalletImport.Dojo previous = chosen;
        chosen = null;
        String message = "";
        if (publicMode.isSelected()) {
            HelperDojoDirectory.Entry entry = dojoChoice.getValue();
            if (entry == null) {
                message = "Choose a Dojo from the list.";
            } else {
                chosen = new HelperWalletImport.Dojo(entry.url(), entry.apiKey());
            }
        } else if (ownMode.isSelected()) {
            boolean typed = !dojoJson.getText().isBlank() || !dojoUrl.getText().isBlank() || !dojoKey.getText().isBlank();
            try {
                chosen = HelperWalletImport.parseDojo(dojoJson.getText(), dojoUrl.getText(), dojoKey.getText());
            } catch (IllegalArgumentException e) {
                message = typed ? e.getMessage() : "Enter the Dojo's URL and API key, or paste its pairing JSON.";
            }
        } else {
            if (payloadText.getText().isBlank()) {
                message = "Paste the pairing payload.";
            } else if (payloadPass.getText().isEmpty()) {
                message = "Enter that wallet's passphrase.";
            } else {
                try {
                    chosen = HelperWalletImport.readPairingPayload(payloadText.getText(), payloadPass.getText()).dojo();
                    if (chosen == null) {
                        message = "That payload has no Dojo in it.";
                    }
                } catch (IllegalArgumentException e) {
                    message = e.getMessage();
                }
            }
        }
        if (previous != null && !previous.equals(chosen)) {
            testedFor = null;
            testResult.setText("");
        }
        messageLabel.setText(message);
        testButton.setDisable(chosen == null || testing);
        boolean tested = chosen != null && keyOf(chosen).equals(testedFor);
        saveButton.setDisable(!tested || walletPassphrase.getText().isEmpty());
        if (tested && walletPassphrase.getText().isEmpty()) {
            messageLabel.setText("Enter your wallet passphrase to save.");
        }
    }

    private static String keyOf(HelperWalletImport.Dojo dojo) {
        return dojo.url() + "\n" + dojo.apiKey();
    }

    @FXML
    private void onTest() {
        HelperWalletImport.Dojo toTest = chosen;
        if (toTest == null) {
            return;
        }
        testing = true;
        testButton.setDisable(true);
        testResult.setText("Testing through Tor, this can take a minute...");
        Thread thread = new Thread(() -> {
            Integer port = HelperTorPort.waitForSocksPort(10_000);
            HelperDojoLogin.Result result = port == null
                    ? new HelperDojoLogin.Result(HelperDojoLogin.Outcome.UNREACHABLE, "Tor is not ready yet.")
                    : HelperDojoLogin.test(toTest.url(), toTest.apiKey(), port);
            later(() -> {
                testing = false;
                testResult.setText(result.message());
                if (result.ok() && toTest.equals(chosen)) {
                    testedFor = keyOf(toTest);
                }
                refresh();
            });
        }, "change-dojo-test");
        thread.setDaemon(true);
        thread.start();
    }

    @FXML
    private void onSave() {
        if (chosen == null || !keyOf(chosen).equals(testedFor)) {
            return;
        }
        String passphrase = walletPassphrase.getText();
        if (!HelperWallet.checkPassphrase(HelperProperties.mnemonicEncrypted, passphrase)) {
            messageLabel.setText("That is not your wallet passphrase.");
            return;
        }
        try {
            HelperProperties.saveDojo(chosen.url(), chosen.apiKey(), passphrase);
            savedUrl = chosen.url();
            stage.close();
        } catch (IOException | RuntimeException e) {
            messageLabel.setText("Could not save the Dojo: " + e.getMessage());
        }
    }

    @FXML
    private void onCancel() {
        stage.close();
    }

    private void clearSecrets() {
        dojoKey.clear();
        dojoJson.clear();
        payloadText.clear();
        payloadPass.clear();
        walletPassphrase.clear();
        chosen = null;
        testedFor = null;
    }
}
