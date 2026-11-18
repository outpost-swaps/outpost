package swap.gui.controller.popups;

import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.Optional;
import javafx.fxml.FXML;
import javafx.fxml.FXMLLoader;
import javafx.scene.Node;
import javafx.scene.Parent;
import javafx.scene.Scene;
import javafx.scene.control.Button;
import javafx.scene.control.CheckBox;
import javafx.scene.control.ComboBox;
import javafx.scene.control.Label;
import javafx.scene.control.PasswordField;
import javafx.scene.control.RadioButton;
import javafx.scene.control.ScrollPane;
import javafx.scene.control.TextArea;
import javafx.scene.control.TextField;
import javafx.scene.image.ImageView;
import javafx.scene.layout.VBox;
import javafx.scene.paint.Color;
import javafx.stage.FileChooser;
import javafx.stage.Modality;
import javafx.stage.Stage;
import javafx.util.StringConverter;
import swap.gui.GUISwap;
import swap.gui.image.FingerprintPicture;
import swap.helper.HelperAshigaruBackup;
import swap.helper.HelperDojoDirectory;
import swap.helper.HelperDojoDirectoryLoader;
import swap.helper.HelperDojoListStatus;
import swap.helper.HelperWalletImport;

/**
 * "Connect your wallet": one window in three steps for a first launch.
 *
 * <ol>
 *   <li><b>Wallet</b>: a pairing payload, an Ashigaru backup file or the seed words. As soon as the
 *       wallet opens, its fingerprint, LifeHash picture and first address are shown to compare with
 *       the real wallet (a wrong passphrase opens a different empty wallet without any error).
 *   <li><b>Dojo</b>: the one that came with the wallet, a public one, or the user's own.
 *   <li><b>Connect</b>: a summary, an app password when the wallet has no passphrase of its own, and
 *       whether to start the ASB.
 * </ol>
 *
 * The result is a pairing record in the shape the pairing screen already uses, so nothing after it
 * changes. Secrets are cleared from the fields when the window closes.
 */
public class PopupConnectWallet {
    /** The pairing record, the passphrase to unlock with, whether to start the ASB, and the fingerprint to show. */
    public record Result(String pairingPayload, String passphrase, boolean startAsb, String fingerprint,
                         swap.helper.HelperServerChoice.Choice servers) {}

    private enum Step { WALLET, DOJO, SERVERS, REVIEW }

    private static final long MAX_BACKUP_BYTES = 2L * 1024 * 1024;

    @FXML private Label titleLabel;
    @FXML private Label stepSubtitle;
    @FXML private Label verifyHint;
    @FXML private VBox confirmShown;
    @FXML private Label stepper1;
    @FXML private Label stepper2;
    @FXML private Label stepper3;
    @FXML private Label stepper4;
    @FXML private ScrollPane serversScroll;
    @FXML private javafx.scene.control.ToggleGroup electrumSource;
    @FXML private RadioButton electrumListed;
    @FXML private RadioButton electrumOwnRadio;
    @FXML private ComboBox<String> electrumChoice;
    @FXML private TextField electrumOwnField;
    @FXML private CheckBox electrumRunsMyself;
    @FXML private CheckBox moneroRunsMyself;
    @FXML private Label moneroRunsMyselfHint;
    @FXML private Button electrumTestButton;
    @FXML private Label electrumTestResult;
    @FXML private javafx.scene.control.ToggleGroup moneroSource;
    @FXML private RadioButton moneroListed;
    @FXML private RadioButton moneroOwnRadio;
    @FXML private ComboBox<String> moneroChoice;
    @FXML private TextField moneroOwnField;
    @FXML private Button moneroTestButton;
    @FXML private Label moneroTestResult;
    @FXML private Label reviewElectrum;
    @FXML private Label reviewMonero;
    @FXML private ScrollPane walletScroll;
    @FXML private ScrollPane dojoScroll;
    @FXML private ScrollPane reviewScroll;

    @FXML private javafx.scene.control.ToggleGroup walletSource;
    @FXML private javafx.scene.control.ToggleGroup networkGroup;
    @FXML private VBox payloadCard;
    @FXML private VBox backupCard;
    @FXML private VBox wordsCard;
    @FXML private RadioButton payloadMode;
    @FXML private RadioButton backupMode;
    @FXML private RadioButton wordsMode;
    @FXML private VBox payloadInputs;
    @FXML private VBox backupInputs;
    @FXML private VBox wordsInputs;
    @FXML private TextArea payloadText;
    @FXML private PasswordField payloadPass;
    @FXML private Label fileLabel;
    @FXML private PasswordField backupPass;
    @FXML private TextArea wordsArea;
    @FXML private PasswordField bip39Pass;
    @FXML private RadioButton testnetRadio;
    @FXML private VBox verifyCard;
    @FXML private VBox createCard;
    @FXML private RadioButton createMode;
    @FXML private VBox createInputs;
    @FXML private javafx.scene.control.ToggleGroup createNetworkGroup;
    @FXML private RadioButton createTestnetRadio;
    @FXML private PasswordField createPass;
    @FXML private PasswordField createPassRepeat;
    @FXML private Button generateButton;
    @FXML private VBox wordsShown;
    @FXML private javafx.scene.layout.GridPane wordsGrid;
    @FXML private TextField confirm1;
    @FXML private TextField confirm2;
    @FXML private TextField confirm3;
    @FXML private ImageView fingerprintImage;
    @FXML private Label fingerprintLabel;
    @FXML private Label addressLabel;

    @FXML private javafx.scene.control.ToggleGroup dojoSource;
    @FXML private VBox walletDojoCard;
    @FXML private VBox publicDojoCard;
    @FXML private VBox ownDojoCard;
    @FXML private RadioButton dojoFromWallet;
    @FXML private RadioButton dojoPublic;
    @FXML private RadioButton dojoOwn;
    @FXML private Label walletDojoUrl;
    @FXML private Label publicDojoHint;
    @FXML private Label dojoListStatus;
    @FXML private VBox publicDojoInputs;
    @FXML private ComboBox<HelperDojoDirectory.Entry> dojoChoice;
    @FXML private VBox ownDojoInputs;
    @FXML private TextArea dojoJson;
    @FXML private TextField dojoUrl;
    @FXML private PasswordField dojoKey;
    @FXML private Button dojoTestButton;
    @FXML private Label dojoTestResult;

    @FXML private ImageView reviewImage;
    @FXML private Label reviewFingerprint;
    @FXML private Label reviewNetwork;
    @FXML private Label reviewAddress;
    @FXML private Label reviewDojo;
    @FXML private VBox appPasswordCard;
    @FXML private PasswordField appPassword;
    @FXML private PasswordField appPasswordRepeat;
    @FXML private CheckBox startAsbBox;
    @FXML private VBox backupExportCard;
    @FXML private Label backupSavedLabel;

    @FXML private Label messageLabel;
    @FXML private Button backButton;
    @FXML private Button nextButton;

    private Stage stage;
    private Result result;
    private Step step = Step.WALLET;
    /** Opened with "Create new wallet": only the create card, and the steps say create, not connect. */
    private boolean creating;
    private String backupText;

    // a wallet being created here: its words (null until generated) and the positions to confirm
    private String createdWords;
    private int[] confirmPositions;

    // the wallet as the inputs describe it; null while they are incomplete or wrong
    private String mnemonic;
    private String bip39;
    private String walletSecret;
    private boolean testnet;
    private boolean secretIsBip39;
    private HelperWalletImport.Verification verification;
    private HelperWalletImport.Dojo walletDojo;

    private HelperWalletImport.Dojo chosenDojo;
    private HelperDojoDirectory.Directory dojoDirectory = HelperDojoDirectory.bundled();
    private HelperDojoDirectoryLoader.Source dojoListSource = HelperDojoDirectoryLoader.Source.SHIPPED;
    private Boolean listedNetwork;
    private boolean updatingDojoChoice;
    private String dojoStepKey;

    // step 3: the servers as chosen; null while a typed address is missing or wrong
    private String chosenElectrum;
    private String chosenMonero;
    private Boolean listedServersNetwork;
    private final java.util.Map<String, String> serverTestText = new java.util.concurrent.ConcurrentHashMap<>();
    private final java.util.Set<String> serverTestFailed = java.util.concurrent.ConcurrentHashMap.newKeySet();
    /** Tests still running, by address, with their start time: shown as a live seconds count. */
    private final java.util.Map<String, Long> serverTestStarted = new java.util.concurrent.ConcurrentHashMap<>();
    private javafx.animation.Timeline serverTestTicker;
    /** Servers that passed a test. Until the user picks one, the first to pass is preselected. */
    private final java.util.Set<String> serverTestPassed = java.util.concurrent.ConcurrentHashMap.newKeySet();
    private boolean electrumPickedByUser;
    private boolean moneroPickedByUser;
    private boolean selectingServer;
    /** Electrum servers taken from Dojo Bay's live directory: offered and tested, preselected only if no curated one works. */
    private final java.util.Set<String> dojoBayElectrum = new java.util.HashSet<>();

    /** Shows the window and waits; empty when the user cancels. */
    public static Optional<Result> show(Stage owner, boolean create) {
        try {
            FXMLLoader loader = new FXMLLoader(GUISwap.class.getResource("/popups/popup-connect-wallet.fxml"));
            Parent root = loader.load();
            Stage stage = new Stage();
            if (owner != null) {
                stage.initOwner(owner);
            }
            stage.initModality(Modality.WINDOW_MODAL);
            stage.setTitle(create ? "Create a wallet" : "Connect your wallet");
            Scene scene = new Scene(root, 720, 740, Color.web("#0e0e0e"));
            GUISwap.applyTheme(scene);
            stage.setScene(scene);

            PopupConnectWallet controller = loader.getController();
            controller.stage = stage;
            controller.creating = create;
            controller.start();
            stage.showAndWait();
            controller.clearSecrets();
            return Optional.ofNullable(controller.result);
        } catch (Exception e) {
            e.printStackTrace();
            return Optional.empty();
        }
    }

    // ---- setup -------------------------------------------------------------------------------

    private void start() {
        walletSource.selectedToggleProperty().addListener((obs, oldVal, newVal) -> {
            syncWalletCards();
            refreshWallet();
        });
        networkGroup.selectedToggleProperty().addListener((obs, oldVal, newVal) -> refreshWallet());
        for (TextField field : new TextField[] {payloadPass, backupPass, bip39Pass}) {
            field.textProperty().addListener((obs, oldVal, newVal) -> refreshWallet());
        }
        payloadText.textProperty().addListener((obs, oldVal, newVal) -> refreshWallet());
        createNetworkGroup.selectedToggleProperty().addListener((obs, oldVal, newVal) -> refreshWallet());
        for (TextField field : new TextField[] {createPass, createPassRepeat, confirm1, confirm2, confirm3}) {
            field.textProperty().addListener((obs, oldVal, newVal) -> refreshWallet());
        }
        wordsArea.textProperty().addListener((obs, oldVal, newVal) -> refreshWallet());

        dojoSource.selectedToggleProperty().addListener((obs, oldVal, newVal) -> {
            syncDojoCards();
            refreshDojo();
        });
        dojoJson.textProperty().addListener((obs, oldVal, newVal) -> refreshDojo());
        dojoUrl.textProperty().addListener((obs, oldVal, newVal) -> refreshDojo());
        dojoKey.textProperty().addListener((obs, oldVal, newVal) -> refreshDojo());
        setupDojoChoice();

        electrumSource.selectedToggleProperty().addListener((obs, oldVal, newVal) -> refreshServers());
        moneroSource.selectedToggleProperty().addListener((obs, oldVal, newVal) -> refreshServers());
        electrumChoice.valueProperty().addListener((obs, oldVal, newVal) -> {
            if (!selectingServer) electrumPickedByUser = true;
            refreshServers();
        });
        moneroChoice.valueProperty().addListener((obs, oldVal, newVal) -> {
            if (!selectingServer) moneroPickedByUser = true;
            refreshServers();
        });
        electrumOwnField.textProperty().addListener((obs, oldVal, newVal) -> refreshServers());
        moneroOwnField.textProperty().addListener((obs, oldVal, newVal) -> refreshServers());

        appPassword.textProperty().addListener((obs, oldVal, newVal) -> refreshReview());
        appPasswordRepeat.textProperty().addListener((obs, oldVal, newVal) -> refreshReview());

        // one window, two flows: connecting shows the three import cards, creating only the create card
        titleLabel.setText(creating ? "Create a wallet" : "Connect your wallet");
        show(payloadCard, !creating);
        show(backupCard, !creating);
        show(wordsCard, !creating);
        show(createCard, creating);
        show(createMode, false); // the card is the whole step: no radio button to pick it
        if (creating) {
            createMode.setSelected(true);
        }
        verifyHint.setText(creating
                ? "Your new wallet. Note the fingerprint: a wallet you restore this seed in shows the same fingerprint and first address."
                : "Compare these with your wallet. A mistyped passphrase opens a different, empty wallet without any error.");

        syncWalletCards();
        selectStep(Step.WALLET);
    }

    private void setupDojoChoice() {
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
            if (!updatingDojoChoice) {
                refreshDojo();
            }
        });
        publicDojoHint.setText("Run by volunteers. A Dojo sees every address in your wallet, so prefer your own or one you trust. "
                + "If one will not connect, pick another.");
        loadDojoList();
    }

    // ---- the public Dojo list: saved or shipped at once, the current one over Tor as soon as Tor is up --------

    private File dojoCacheFile() {
        try {
            return new File(swap.lib.AppSwap.getSwapRootDir(), "dojo-directory-cache.json");
        } catch (Throwable noDataDirYet) {
            return null;
        }
    }

    private void loadDojoList() {
        File cache = dojoCacheFile();
        HelperDojoDirectoryLoader.Loaded offline = HelperDojoDirectoryLoader.loadOffline(cache);
        dojoDirectory = offline.directory();
        dojoListSource = offline.source();
        dojoListStatus.setText(HelperDojoListStatus.text(offline.source(), offline.directory(), HelperDojoListStatus.Phase.LOADING, null));

        Thread fetch = new Thread(() -> {
            Integer port = swap.helper.HelperTorPort.waitForSocksPort(90_000);
            if (port == null) {
                later(() -> dojoListStatus.setText(HelperDojoListStatus.text(
                        dojoListSource, dojoDirectory, HelperDojoListStatus.Phase.FAILED, "Tor is not ready")));
                return;
            }
            try {
                HelperDojoDirectoryLoader.Loaded live = HelperDojoDirectoryLoader.loadLive(port, cache);
                later(() -> {
                    dojoDirectory = live.directory();
                    dojoListSource = live.source();
                    dojoListStatus.setText(HelperDojoListStatus.text(live.source(), live.directory(), HelperDojoListStatus.Phase.DONE, null));
                    relistDojos();
                    addDojoBayElectrum(); // servers from the live list, if the Servers step is open already
                });
            } catch (Exception e) {
                String reason = HelperDojoDirectoryLoader.reason(e);
                later(() -> dojoListStatus.setText(HelperDojoListStatus.text(
                        dojoListSource, dojoDirectory, HelperDojoListStatus.Phase.FAILED, reason)));
            }
        }, "dojo-directory-fetch");
        fetch.setDaemon(true);
        fetch.start();
    }

    /** Runs on the UI thread, and only while the window is still open. */
    private void later(Runnable action) {
        javafx.application.Platform.runLater(() -> {
            if (stage != null && stage.isShowing()) {
                action.run();
            }
        });
    }

    /** A fresher list arrived: refill the drop-down for the wallet's network, keeping the chosen Dojo if it is still listed. */
    private void relistDojos() {
        String chosenId = dojoChoice.getValue() == null ? null : dojoChoice.getValue().id();
        listedNetwork = null;
        if (verification != null) {
            listDojos(testnet);
            if (chosenId != null) {
                for (HelperDojoDirectory.Entry entry : dojoChoice.getItems()) {
                    if (entry.id().equals(chosenId)) {
                        dojoChoice.setValue(entry);
                    }
                }
            }
        }
        refreshDojo();
    }

    private void listDojos(boolean testnetWallet) {
        if (listedNetwork != null && listedNetwork == testnetWallet) {
            return;
        }
        listedNetwork = testnetWallet;
        updatingDojoChoice = true;
        try {
            dojoChoice.getItems().setAll(dojoDirectory.forNetwork(testnetWallet));
            dojoChoice.setValue(null);
        } finally {
            updatingDojoChoice = false;
        }
    }

    // ---- step 1: the wallet -------------------------------------------------------------------

    private static void show(Node node, boolean visible) {
        node.setVisible(visible);
        node.setManaged(visible);
    }

    private static void selected(VBox card, boolean selected) {
        card.getStyleClass().remove("option-card-selected");
        if (selected) {
            card.getStyleClass().add("option-card-selected");
        }
    }

    private void syncWalletCards() {
        show(payloadInputs, payloadMode.isSelected());
        show(backupInputs, backupMode.isSelected());
        show(wordsInputs, wordsMode.isSelected());
        show(createInputs, createMode.isSelected());
        selected(payloadCard, payloadMode.isSelected());
        selected(backupCard, backupMode.isSelected());
        selected(wordsCard, wordsMode.isSelected());
        selected(createCard, createMode.isSelected());
        if (!createMode.isSelected()) {
            forgetCreatedWords(); // never leave new words on screen or in memory behind another card
        }
    }

    // ---- creating a wallet: 12 words and a required passphrase, as Ashigaru does --------------

    @FXML
    private void onGenerateWords() {
        try {
            createdWords = HelperWalletImport.generateWords();
        } catch (IllegalStateException e) {
            // fail closed: never show words from a generator that looks broken
            forgetCreatedWords();
            messageLabel.setText(e.getMessage());
            return;
        }
        confirmPositions = HelperWalletImport.confirmationPositions(new java.security.SecureRandom());
        String[] words = createdWords.split(" ");
        wordsGrid.getChildren().clear();
        for (int i = 0; i < words.length; i++) {
            Label word = new Label(String.format("%2d  %s", i + 1, words[i]));
            word.getStyleClass().add("import-mono-large");
            wordsGrid.add(word, i % 3, i / 3); // read across: 1 2 3 / 4 5 6 ...
        }
        TextField[] fields = {confirm1, confirm2, confirm3};
        for (int i = 0; i < fields.length; i++) {
            fields[i].clear();
            fields[i].setPromptText("Word #" + (confirmPositions[i] + 1));
        }
        generateButton.setText("New words");
        show(wordsShown, true);
        show(confirmShown, false);
        refreshWallet();
    }

    /** The words go off screen while they are typed back from paper. */
    @FXML
    private void onWordsWrittenDown() {
        show(wordsShown, false);
        show(confirmShown, true);
        confirm1.requestFocus();
        refreshWallet();
    }

    @FXML
    private void onShowWordsAgain() {
        show(confirmShown, false);
        show(wordsShown, true);
        refreshWallet();
    }

    private void forgetCreatedWords() {
        createdWords = null;
        confirmPositions = null;
        if (wordsGrid != null) {
            wordsGrid.getChildren().clear();
            show(wordsShown, false);
            show(confirmShown, false);
            generateButton.setText("Generate words");
            for (TextField field : new TextField[] {confirm1, confirm2, confirm3}) {
                field.clear();
            }
        }
    }

    @FXML
    private void onChooseFile() {
        FileChooser chooser = new FileChooser();
        chooser.setTitle("Choose the Ashigaru backup file");
        chooser.getExtensionFilters().addAll(
                new FileChooser.ExtensionFilter("Backup files", "*.txt", "*.json", "*.dat"),
                new FileChooser.ExtensionFilter("All files", "*.*"));
        File file = chooser.showOpenDialog(stage);
        if (file == null) {
            return;
        }
        try {
            if (file.length() > MAX_BACKUP_BYTES) {
                backupText = null;
                fileLabel.setText("That file is too large to be a backup.");
            } else {
                backupText = new String(Files.readAllBytes(file.toPath()), StandardCharsets.UTF_8);
                fileLabel.setText(file.getName());
            }
        } catch (Exception e) {
            backupText = null;
            fileLabel.setText("Could not read that file.");
        }
        refreshWallet();
    }

    private void refreshWallet() {
        mnemonic = null;
        bip39 = null;
        walletSecret = null;
        walletDojo = null;
        verification = null;
        secretIsBip39 = false;
        String message = "";

        if (payloadMode.isSelected()) {
            if (payloadText.getText().isBlank()) {
                message = "Paste the pairing payload.";
            } else if (payloadPass.getText().isEmpty()) {
                message = "Enter the wallet's passphrase.";
            } else {
                try {
                    HelperWalletImport.PayloadRead read = HelperWalletImport.readPairingPayload(payloadText.getText(), payloadPass.getText());
                    mnemonic = read.mnemonic();
                    bip39 = read.bip39Passphrase();
                    testnet = read.testnet();
                    walletSecret = payloadPass.getText();
                    secretIsBip39 = read.hasPassphrase();
                    walletDojo = read.dojo();
                } catch (IllegalArgumentException e) {
                    message = e.getMessage();
                }
            }
        } else if (backupMode.isSelected()) {
            if (backupText == null) {
                message = "Choose the backup file.";
            } else if (backupPass.getText().isEmpty()) {
                message = "Enter the wallet's passphrase.";
            } else {
                try {
                    HelperAshigaruBackup.Imported imported = HelperAshigaruBackup.read(backupText, backupPass.getText());
                    mnemonic = imported.mnemonic();
                    bip39 = imported.passphrase();
                    testnet = imported.testnet();
                    walletSecret = backupPass.getText();
                    secretIsBip39 = !bip39.isEmpty();
                    if (imported.dojoUrl() != null && imported.dojoApiKey() != null) {
                        walletDojo = new HelperWalletImport.Dojo(imported.dojoUrl(), imported.dojoApiKey());
                    }
                } catch (HelperAshigaruBackup.BackupException e) {
                    message = e.getMessage();
                }
            }
        } else if (createMode.isSelected()) {
            String problem = HelperWalletImport.newWalletPassphraseProblem(createPass.getText(), createPassRepeat.getText());
            if (problem != null) {
                message = problem;
            } else if (createdWords == null) {
                message = "Generate the words and write them down.";
            } else if (!confirmShown.isVisible()) {
                message = "Write the 12 words on paper, then press \"I wrote them down\".";
            } else if (!HelperWalletImport.confirmationMatches(createdWords, confirmPositions,
                    new String[] {confirm1.getText(), confirm2.getText(), confirm3.getText()})) {
                message = "Type words #" + (confirmPositions[0] + 1) + ", #" + (confirmPositions[1] + 1)
                        + " and #" + (confirmPositions[2] + 1) + " to show you wrote them down.";
            } else {
                mnemonic = createdWords;
                bip39 = createPass.getText();
                testnet = createTestnetRadio.isSelected();
                secretIsBip39 = true;
                walletSecret = bip39;
            }
        } else {
            String words = HelperWalletImport.normalizeWords(wordsArea.getText());
            String problem = words.isEmpty() ? "Enter the wallet's seed words." : HelperWalletImport.checkWords(words);
            if (problem != null) {
                message = problem;
            } else {
                mnemonic = words;
                bip39 = bip39Pass.getText();
                testnet = testnetRadio.isSelected();
                secretIsBip39 = !bip39.isEmpty();
                walletSecret = bip39;
            }
        }

        if (mnemonic != null) {
            verification = HelperWalletImport.verify(mnemonic, bip39, testnet);
        }
        showVerification();
        if (step == Step.WALLET) {
            messageLabel.setText(message);
            nextButton.setDisable(mnemonic == null);
        }
    }

    private void showVerification() {
        boolean found = verification != null;
        show(verifyCard, found);
        if (found) {
            fingerprintLabel.setText(verification.fingerprintHex());
            addressLabel.setText(verification.firstReceiveAddress());
            fingerprintImage.setImage(FingerprintPicture.of(verification.fingerprint(), 64));
        } else {
            fingerprintImage.setImage(null);
        }
    }

    /** Words with no BIP39 passphrase have no secret of their own: the user chooses an app password. */
    private boolean needsAppPassword() {
        return wordsMode.isSelected() && bip39 != null && bip39.isEmpty();
    }

    // ---- step 2: the Dojo ---------------------------------------------------------------------

    private void syncDojoCards() {
        show(publicDojoInputs, dojoPublic.isSelected());
        show(ownDojoInputs, dojoOwn.isSelected());
        selected(walletDojoCard, dojoFromWallet.isSelected());
        selected(publicDojoCard, dojoPublic.isSelected());
        selected(ownDojoCard, dojoOwn.isSelected());
    }

    private void enterDojoStep() {
        listDojos(testnet);
        boolean fromWallet = walletDojo != null;
        show(walletDojoCard, fromWallet);
        if (fromWallet) {
            walletDojoUrl.setText(walletDojo.url());
        }
        // a different wallet or Dojo than last time: start from the sensible default again
        String key = verification.fingerprintHex() + "|" + (walletDojo == null ? "" : walletDojo.url());
        if (!key.equals(dojoStepKey)) {
            dojoStepKey = key;
            dojoChoice.setValue(null);
            if (fromWallet) {
                dojoFromWallet.setSelected(true);
            } else {
                dojoPublic.setSelected(true);
            }
        } else if (!fromWallet && dojoFromWallet.isSelected()) {
            dojoPublic.setSelected(true);
        }
        syncDojoCards();
        refreshDojo();
    }

    private void refreshDojo() {
        chosenDojo = null;
        String message = "";
        if (dojoFromWallet.isSelected() && walletDojo != null) {
            chosenDojo = walletDojo;
        } else if (dojoPublic.isSelected()) {
            HelperDojoDirectory.Entry entry = dojoChoice.getValue();
            if (entry == null) {
                message = dojoDirectory.entries().isEmpty() ? "No public Dojos are bundled; use your own." : "Choose a Dojo from the list.";
            } else {
                chosenDojo = new HelperWalletImport.Dojo(entry.url(), entry.apiKey());
            }
        } else if (dojoOwn.isSelected()) {
            boolean typed = !dojoJson.getText().isBlank() || !dojoUrl.getText().isBlank() || !dojoKey.getText().isBlank();
            try {
                chosenDojo = HelperWalletImport.parseDojo(dojoJson.getText(), dojoUrl.getText(), dojoKey.getText());
            } catch (IllegalArgumentException e) {
                message = typed ? e.getMessage() : "Enter the Dojo's URL and API key, or paste its pairing JSON.";
            }
        }
        String dojoKeyForTest = chosenDojo == null ? null : chosenDojo.url();
        dojoTestButton.setDisable(dojoKeyForTest == null || serverTestStarted.containsKey("dojo:" + dojoKeyForTest));
        dojoTestResult.setText(testText(dojoKeyForTest == null ? null : "dojo:" + dojoKeyForTest));
        dojoTestResult.setStyle(testStyle(dojoKeyForTest == null ? null : "dojo:" + dojoKeyForTest));
        if (step == Step.DOJO) {
            if (message.isEmpty() && dojoKeyForTest != null && serverTestFailed.contains("dojo:" + dojoKeyForTest)) {
                message = "This Dojo did not pass the test. Pick another, or continue and change it later in Settings.";
            }
            messageLabel.setText(message);
            nextButton.setDisable(chosenDojo == null);
        }
    }

    /** Logs in to the chosen Dojo over Tor with its URL and API key, as Settings' Dojo test does. */
    @FXML
    private void onTestDojo() {
        HelperWalletImport.Dojo dojo = chosenDojo;
        if (dojo == null) {
            return;
        }
        testServer("dojo:" + dojo.url(), () -> {
            long started = System.currentTimeMillis();
            swap.helper.HelperDojoLogin.Result r = swap.helper.HelperDojoLogin.test(dojo.url(), dojo.apiKey(), socksPortOrFail());
            if (r.ok()) {
                return passed(System.currentTimeMillis() - started, "logged in");
            }
            return failed(switch (r.outcome()) {
                case BAD_KEY -> "the API key was rejected";
                case UNREACHABLE -> "could not reach it over Tor";
                default -> "unexpected reply";
            });
        });
    }

    // ---- step 3: the Electrum server and Monero node -------------------------------------------

    private static <T> java.util.List<String> urls(java.util.List<T> nodes, java.util.function.Function<T, String> url) {
        java.util.List<String> out = new java.util.ArrayList<>();
        for (T node : nodes) {
            String u = url.apply(node);
            if (!out.contains(u)) {
                out.add(u);
            }
        }
        return out;
    }

    private void enterServersStep() {
        if (listedServersNetwork == null || listedServersNetwork != testnet) {
            listedServersNetwork = testnet;
            // the same curated lists the app uses over Tor (AppSwap): onion first for Electrum
            swap.model.ElectrumServer.Network btcNetwork = testnet ? swap.model.ElectrumServer.Network.TESTNET : swap.model.ElectrumServer.Network.MAINNET;
            java.util.List<String> electrum = urls(concat(swap.helper.NodeDirectory.electrum(btcNetwork, true),
                    swap.helper.NodeDirectory.electrum(btcNetwork, false)), swap.model.ElectrumServer::url);
            swap.model.XmrNode.Network xmrNetwork = testnet ? swap.model.XmrNode.Network.STAGENET : swap.model.XmrNode.Network.MAINNET;
            java.util.List<String> monero = urls(concat(swap.helper.NodeDirectory.monero(xmrNetwork, false),
                    swap.helper.NodeDirectory.monero(xmrNetwork, true)), swap.model.XmrNode::url);
            selectingServer = true;
            try {
                electrumChoice.getItems().setAll(electrum);
                moneroChoice.getItems().setAll(monero);
                java.util.Random random = new java.security.SecureRandom();
                electrumChoice.setValue(swap.helper.HelperServerChoice.defaultElectrum(electrum, random));
                moneroChoice.setValue(monero.isEmpty() ? null : swap.helper.HelperDaemonPool.pickDefault(monero, random));
            } finally {
                selectingServer = false;
            }
            electrumPickedByUser = false;
            moneroPickedByUser = false;
            // test every listed server at once; the first to answer is preselected (servers come and go,
            // and a random pick drew a dead one on two first launches in a row, 2026-11-12)
            for (String url : electrum) {
                testElectrumUrl(url);
            }
            addDojoBayElectrum();
            for (String url : monero) {
                testMoneroUrl(url);
            }
        }
        refreshServers();
    }

    /**
     * Adds the Electrum servers of verified Dojos in Dojo Bay's directory (live when it has arrived,
     * else the saved or shipped copy) after the curated ones, and tests them. Their addresses are not
     * covered by the directory's signatures, so each must pass the same test as any other server.
     */
    private void addDojoBayElectrum() {
        if (listedServersNetwork == null) {
            return; // the Servers step has not been opened yet
        }
        for (swap.helper.HelperElectrumDiscovery.Candidate candidate : swap.helper.HelperElectrumDiscovery.candidates(dojoDirectory, testnet)) {
            if (!electrumChoice.getItems().contains(candidate.url())) {
                electrumChoice.getItems().add(candidate.url());
                dojoBayElectrum.add(candidate.url());
                testElectrumUrl(candidate.url());
            }
        }
    }

    /**
     * A server passed: preselect it if the user has not picked one and the current one has not
     * passed. A curated server is preferred: a Dojo Bay one is chosen only while no curated one
     * works, and gives way when one does.
     */
    private void preferWorkingServer(String url) {
        selectingServer = true;
        try {
            String current = electrumChoice.getValue();
            boolean curated = !dojoBayElectrum.contains(url);
            boolean currentIsFallback = current != null && dojoBayElectrum.contains(current);
            if (!electrumPickedByUser && !electrumOwnRadio.isSelected() && electrumChoice.getItems().contains(url)
                    && (!serverTestPassed.contains(current) || (curated && currentIsFallback))) {
                electrumChoice.setValue(url);
            }
            if (!moneroPickedByUser && !moneroOwnRadio.isSelected() && moneroChoice.getItems().contains(url)
                    && !serverTestPassed.contains(moneroChoice.getValue())) {
                moneroChoice.setValue(url);
            }
        } finally {
            selectingServer = false;
        }
    }

    private static <T> java.util.List<T> concat(java.util.List<T> a, java.util.List<T> b) {
        java.util.List<T> out = new java.util.ArrayList<>(a);
        out.addAll(b);
        return out;
    }

    private void refreshServers() {
        boolean electrumOwn = electrumOwnRadio.isSelected();
        boolean moneroOwn = moneroOwnRadio.isSelected();
        show(electrumChoice, !electrumOwn);
        show(electrumOwnField, electrumOwn);
        show(moneroChoice, !moneroOwn);
        show(moneroOwnField, moneroOwn);
        show(electrumRunsMyself, electrumOwn);
        show(moneroRunsMyself, moneroOwn);
        show(moneroRunsMyselfHint, moneroOwn);

        String message = "";
        chosenElectrum = null;
        chosenMonero = null;
        try {
            chosenElectrum = electrumOwn
                    ? (electrumOwnField.getText().isBlank() ? null : swap.helper.HelperServerChoice.normalizeElectrum(electrumOwnField.getText()))
                    : electrumChoice.getValue();
        } catch (IllegalArgumentException e) {
            message = e.getMessage();
        }
        try {
            chosenMonero = moneroOwn
                    ? (moneroOwnField.getText().isBlank() ? null : swap.helper.HelperServerChoice.normalizeMonero(moneroOwnField.getText()))
                    : moneroChoice.getValue();
        } catch (IllegalArgumentException e) {
            if (message.isEmpty()) {
                message = e.getMessage();
            }
        }
        if (message.isEmpty() && chosenElectrum == null) {
            message = electrumOwn ? "Enter the Electrum server." : "Choose an Electrum server.";
        } else if (message.isEmpty() && chosenMonero == null) {
            message = moneroOwn ? "Enter the Monero node." : "Choose a Monero node.";
        } else if (message.isEmpty() && (serverTestFailed.contains(chosenElectrum) || serverTestFailed.contains(chosenMonero))) {
            message = "A server did not pass the test. Pick another, or continue and change it later in Settings.";
        }
        electrumTestResult.setText(testText(chosenElectrum));
        moneroTestResult.setText(testText(chosenMonero));
        electrumTestResult.setStyle(testStyle(chosenElectrum));
        moneroTestResult.setStyle(testStyle(chosenMonero));
        electrumTestButton.setDisable(chosenElectrum == null || serverTestStarted.containsKey(chosenElectrum));
        moneroTestButton.setDisable(chosenMonero == null || serverTestStarted.containsKey(chosenMonero));
        if (step == Step.SERVERS) {
            messageLabel.setText(message);
            nextButton.setDisable(chosenElectrum == null || chosenMonero == null);
        }
    }

    @FXML
    private void onTestElectrum() {
        if (chosenElectrum != null) {
            testElectrumUrl(chosenElectrum);
        }
    }

    private void testElectrumUrl(String url) {
        boolean testnetWallet = testnet;
        testServer(url, () -> {
            swap.helper.HelperElectrumProbe.Result r = swap.helper.HelperElectrumProbe.probe(url, testnetWallet,
                    swap.helper.HelperElectrumProbe.throughTor(socksPortOrFail()), 60_000);
            return r.ok() ? passed(r.millis(), "block " + r.height()) : failed(r.reason());
        });
    }

    @FXML
    private void onTestMonero() {
        if (chosenMonero != null) {
            testMoneroUrl(chosenMonero);
        }
    }

    private void testMoneroUrl(String url) {
        boolean stagenet = testnet;
        testServer(url, () -> {
            swap.helper.HelperMoneroProbe.Result r = swap.helper.HelperMoneroProbe.probe(url, stagenet,
                    swap.helper.HelperElectrumProbe.throughTor(socksPortOrFail()), 60_000);
            return r.ok() ? passed(r.millis(), "block " + r.height()) : failed(r.reason());
        });
    }

    private String testText(String url) {
        if (url == null) {
            return "";
        }
        Long started = serverTestStarted.get(url);
        if (started != null) {
            long seconds = (System.currentTimeMillis() - started) / 1000;
            return "Testing over Tor... " + seconds + " s. A first connection to an onion can take up to a minute.";
        }
        return serverTestText.getOrDefault(url, "");
    }

    /** Pass and fail in the same green and red as Settings' Dojo test; plain while testing. */
    private String testStyle(String url) {
        if (url == null || serverTestStarted.containsKey(url) || !serverTestText.containsKey(url)) {
            return "";
        }
        return "-fx-font-weight: bold; -fx-text-fill: " + (serverTestFailed.contains(url) ? "#e5534b" : "#3fb950") + ";";
    }

    /** Ticks once a second while any test runs, so the seconds count moves. */
    private void keepTestTickerRunning() {
        if (serverTestTicker == null) {
            serverTestTicker = new javafx.animation.Timeline(new javafx.animation.KeyFrame(javafx.util.Duration.seconds(1), e -> {
                if (serverTestStarted.isEmpty()) {
                    serverTestTicker.stop();
                }
                refreshServers();
                refreshDojo();
            }));
            serverTestTicker.setCycleCount(javafx.animation.Animation.INDEFINITE);
        }
        serverTestTicker.play();
    }

    private static int socksPortOrFail() {
        Integer port = swap.helper.HelperTorPort.waitForSocksPort(90_000);
        if (port == null) {
            throw new IllegalStateException("Tor is not ready yet");
        }
        return port;
    }

    /**
     * A typed-in server is the user's own only when they say they run it. "Another node" alone stays
     * public: an own Monero node is trusted alone to confirm the seller's lock, with no witnesses
     * (funds-risk review 2026-11-13).
     */
    private boolean electrumIsOwn() {
        return electrumOwnRadio.isSelected() && electrumRunsMyself.isSelected();
    }

    private boolean moneroIsOwn() {
        return moneroOwnRadio.isSelected() && moneroRunsMyself.isSelected();
    }

    private static String passed(long millis, String detail) {
        return "\u2713 Answered in " + String.format(java.util.Locale.ROOT, "%.1f", millis / 1000.0) + " s (" + detail + ").";
    }

    private static String failed(String reason) {
        if ("timed out".equals(reason)) {
            return "\u2717 No answer within 60 s. Tor may still be finding a route: test again, or pick another.";
        }
        return "\u2717 Did not pass: " + (reason == null || reason.isBlank() ? "no answer" : reason) + ".";
    }

    /** Runs one test over Tor off the UI thread; the result is kept per address. */
    private void testServer(String url, java.util.concurrent.Callable<String> test) {
        if (serverTestStarted.putIfAbsent(url, System.currentTimeMillis()) != null) {
            return; // already being tested
        }
        serverTestFailed.remove(url);
        keepTestTickerRunning();
        refreshServers();
        refreshDojo();
        Thread worker = new Thread(() -> {
            String text;
            try {
                text = test.call();
            } catch (Exception e) {
                text = failed(e.getMessage());
            }
            String result = text;
            later(() -> {
                serverTestStarted.remove(url);
                serverTestText.put(url, result);
                if (result.startsWith("\u2717")) {
                    serverTestFailed.add(url);
                    serverTestPassed.remove(url);
                } else {
                    serverTestPassed.add(url);
                    preferWorkingServer(url);
                }
                refreshServers();
                refreshDojo();
            });
        }, "server-test");
        worker.setDaemon(true);
        worker.start();
    }

    // ---- step 4: connect ----------------------------------------------------------------------

    private void enterReviewStep() {
        reviewImage.setImage(FingerprintPicture.of(verification.fingerprint(), 56));
        reviewFingerprint.setText(verification.fingerprintHex());
        reviewNetwork.setText(testnet ? "Testnet" : "Mainnet");
        reviewAddress.setText(verification.firstReceiveAddress());
        reviewDojo.setText(chosenDojo.url());
        reviewElectrum.setText(chosenElectrum + (electrumIsOwn() ? "  (you run it)" : ""));
        reviewMonero.setText(chosenMonero + (moneroIsOwn() ? "  (you run it, trusted alone)" : ""));
        show(appPasswordCard, needsAppPassword());
        show(backupExportCard, createMode.isSelected());
        backupSavedLabel.setText("");
        refreshReview();
    }

    private void refreshReview() {
        if (step != Step.REVIEW) {
            return;
        }
        String message = "";
        boolean ready = true;
        if (needsAppPassword()) {
            String problem = HelperWalletImport.appPasswordProblem(appPassword.getText(), appPasswordRepeat.getText());
            if (problem != null) {
                message = problem;
                ready = false;
            }
        }
        messageLabel.setText(message);
        nextButton.setDisable(!ready);
    }

    // ---- navigation ---------------------------------------------------------------------------

    private void selectStep(Step next) {
        step = next;
        show(walletScroll, next == Step.WALLET);
        show(dojoScroll, next == Step.DOJO);
        show(serversScroll, next == Step.SERVERS);
        show(reviewScroll, next == Step.REVIEW);
        show(backButton, next != Step.WALLET);
        nextButton.setText(next != Step.REVIEW ? "Next" : creating ? "Create wallet" : "Connect");
        stepSubtitle.setText(switch (next) {
            case WALLET -> creating
                    ? "Choose a strong passphrase, then write down your 12 new words."
                    : "Choose how to bring in your wallet.";
            case DOJO -> "Your balances and fee estimates come from a Dojo server.";
            case SERVERS -> "Choose the Electrum server and Monero node, or use your own.";
            case REVIEW -> creating ? "Check the details, then create the wallet." : "Check the details, then connect.";
        });
        styleStepper(stepper1, "1  WALLET", 0, next);
        styleStepper(stepper2, "2  DOJO", 1, next);
        styleStepper(stepper3, "3  SERVERS", 2, next);
        styleStepper(stepper4, creating ? "4  CREATE" : "4  CONNECT", 3, next);
        switch (next) {
            case WALLET -> refreshWallet();
            case DOJO -> enterDojoStep();
            case SERVERS -> enterServersStep();
            case REVIEW -> enterReviewStep();
        }
    }

    private static void styleStepper(Label label, String text, int index, Step current) {
        label.getStyleClass().removeAll("stepper-current", "stepper-done");
        int currentIndex = current.ordinal();
        if (index < currentIndex) {
            label.getStyleClass().add("stepper-done");
            label.setText("✓  " + text.substring(3));
        } else {
            if (index == currentIndex) {
                label.getStyleClass().add("stepper-current");
            }
            label.setText(text);
        }
    }

    @FXML
    private void onNext() {
        switch (step) {
            case WALLET -> {
                if (mnemonic != null) {
                    selectStep(Step.DOJO);
                }
            }
            case DOJO -> {
                if (chosenDojo != null) {
                    selectStep(Step.SERVERS);
                }
            }
            case SERVERS -> {
                if (chosenElectrum != null && chosenMonero != null) {
                    selectStep(Step.REVIEW);
                }
            }
            case REVIEW -> finish();
        }
    }

    @FXML
    private void onBack() {
        selectStep(switch (step) {
            case REVIEW -> Step.SERVERS;
            case SERVERS -> Step.DOJO;
            default -> Step.WALLET;
        });
    }

    @FXML
    private void onSaveBackup() {
        if (mnemonic == null || bip39 == null || bip39.isEmpty()) {
            return;
        }
        FileChooser chooser = new FileChooser();
        chooser.setTitle("Save the Ashigaru backup file");
        chooser.setInitialFileName("ashigaru.txt");
        File file = chooser.showSaveDialog(stage);
        if (file == null) {
            return;
        }
        try {
            String backup = swap.helper.HelperAshigaruBackup.write(mnemonic, bip39, testnet, chosenDojo);
            Files.write(file.toPath(), backup.getBytes(StandardCharsets.UTF_8));
            backupSavedLabel.setText("Saved to " + file.getName() + ".");
        } catch (Exception e) {
            backupSavedLabel.setText("Could not save the file: " + e.getMessage());
        }
    }

    @FXML
    private void onCancel() {
        stage.close();
    }

    private void finish() {
        if (needsAppPassword()) {
            String problem = HelperWalletImport.appPasswordProblem(appPassword.getText(), appPasswordRepeat.getText());
            if (problem != null) {
                messageLabel.setText(problem);
                return;
            }
        }
        String secret = needsAppPassword() ? appPassword.getText() : walletSecret;
        if (secret == null || secret.isEmpty()) {
            messageLabel.setText("Choose a password for this app.");
            return;
        }
        String payload = HelperWalletImport.buildPairingPayload(mnemonic, secret, secretIsBip39, testnet, chosenDojo);
        result = new Result(payload, secret, startAsbBox.isSelected(), verification.fingerprintHex(),
                new swap.helper.HelperServerChoice.Choice(chosenElectrum, electrumIsOwn(), chosenMonero, moneroIsOwn()));
        stage.close();
    }

    private void clearSecrets() {
        payloadText.clear();
        payloadPass.clear();
        backupPass.clear();
        wordsArea.clear();
        bip39Pass.clear();
        dojoKey.clear();
        dojoJson.clear();
        appPassword.clear();
        appPasswordRepeat.clear();
        createPass.clear();
        createPassRepeat.clear();
        forgetCreatedWords();
        backupText = null;
        mnemonic = null;
        bip39 = null;
        walletSecret = null;
    }
}
