package swap.gui;

import atlantafx.base.theme.*;
import com.samourai.http.client.HttpUsage;
import javafx.application.Application;
import javafx.fxml.FXMLLoader;
import javafx.scene.Scene;
import javafx.scene.image.Image;
import javafx.scene.paint.Color;
import javafx.stage.Stage;
import org.berndpruenster.netlayer.tor.HsContainer;
import org.berndpruenster.netlayer.tor.NativeTor;
import org.berndpruenster.netlayer.tor.Tor;
import org.berndpruenster.netlayer.tor.TorCtlException;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import swap.gui.controller.MainController;
import swap.gui.controller.PairingController;
import swap.gui.controller.pages.SwapsController;
import swap.gui.controller.popups.PopupActiveSwapWarning;
import swap.helper.*;
import swap.lib.AppAsb;
import swap.lib.AppPriceTicker;
import swap.lib.AppSwap;
import swap.wallet.SwapsWallet;

import java.io.File;
import java.io.IOException;
import java.lang.invoke.MethodHandles;
import java.net.URL;
import java.nio.file.Files;
import java.util.EnumMap;
import java.util.Map;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

public class GUISwap extends Application {
    private static final Logger log = LoggerFactory.getLogger(MethodHandles.lookup().lookupClass());
    public static final String VERSION = "0.1.0-beta";
    public static final String TOR_BROWSER_VERSION = "13.0.6";
    public static AtomicBoolean running = new AtomicBoolean(false);
    public static AppSwap appSwap = null;
    public static AppAsb appAsb = new AppAsb();
    public static AppPriceTicker appPriceTicker = null;
    public static ExecutorService executorService = Executors.newCachedThreadPool(new HelperThread());
    public static ScheduledExecutorService scheduledExecutorService = Executors.newScheduledThreadPool(32, new HelperThread());
    public static HsContainer hiddenServiceContainer = null;
    private static GUISwap instance = null;

    // A single cleanup thread owns shutdown. The close handler, Application.stop()
    // and the JVM shutdown hook all funnel here so a second caller waits for the
    // first instead of racing it (the previous code could halt the JVM before the
    // sidecars were reaped).
    private static final AtomicBoolean shuttingDown = new AtomicBoolean(false);
    private static final CountDownLatch shutdownComplete = new CountDownLatch(1);
    private static final long SHUTDOWN_WAIT_SECONDS = 60;

    // One NativeTor per HttpUsage for Whirlpool traffic — separate circuits so REST,
    // websocket, backend and output-registration traffic aren't correlatable via a shared
    // connection. Started lazily (see startWhirlpoolTorClients) only when mixing is
    // actually used, not unconditionally at app startup — ~50MB+ RAM per instance.
    private static final Map<HttpUsage, NativeTor> whirlpoolTorClients = new EnumMap<>(HttpUsage.class);
    private static final AtomicBoolean whirlpoolTorStarted = new AtomicBoolean(false);

    public static Map<HttpUsage, NativeTor> getWhirlpoolTorClients() {
        return whirlpoolTorClients;
    }

    /**
     * Blocking — spawns 4 separate Tor instances if not already started. Call from a
     * background thread. Safe to call multiple times: only retries usages that are still
     * missing (e.g. a prior partial failure), and only latches "started" once every usage
     * has actually succeeded — a single transient failure must not permanently disable
     * mixing for the rest of the session.
     */
    public static synchronized void startWhirlpoolTorClients() {
        if (whirlpoolTorStarted.get()) return;
        File swapRootDir = AppSwap.getSwapRootDir();
        File torDir = new File(swapRootDir, "tor-v" + TOR_BROWSER_VERSION);
        boolean allStarted = true;
        for (HttpUsage usage : HttpUsage.values()) {
            if (whirlpoolTorClients.containsKey(usage)) continue; // already started
            try {
                File usageDir = new File(torDir, "whirlpool-clients/tor-whirlpool-" + usage.name().toLowerCase());
                if (!usageDir.exists()) usageDir.mkdirs();
                whirlpoolTorClients.put(usage, new NativeTor(usageDir));
            } catch (TorCtlException e) {
                log.warn("Failed to start Tor instance for {}: {}", usage, e.getMessage());
                allStarted = false;
            }
        }
        whirlpoolTorStarted.set(allStarted);
    }

    public static GUISwap getInstance() {
        return instance;
    }

    public static void startGui(String[] args) {
        running.set(true);

        HelperLibraryNative.loadLibrary();
        HelperXmrNodesProperties.getInstance();
        HelperBtcNodesProperties.getInstance();
        HelperRendezvousPeersProperties.getInstance();

        File swapRootDir = AppSwap.getSwapRootDir();
        if (!swapRootDir.exists()) swapRootDir.mkdirs();
        HelperDatabaseBackup.backupDaily(swapRootDir);
        maybeUpgradeTorFolder(swapRootDir);
        File torDir = new File(swapRootDir, "tor-v" + TOR_BROWSER_VERSION);

        executorService.submit(() -> {
            try {
                Tor.setDefault(new NativeTor(torDir));
            } catch (TorCtlException e) {
                // Fail closed, but loudly: without Tor, swap discovery and execution
                // will not work — make that visible instead of a silent spinner.
                e.printStackTrace();
                MainController mainController = MainController.getInstance();
                if (mainController != null) {
                    mainController.printSwapLogLn(swap.model.LogType.ERROR,
                            "Tor failed to start: " + e.getMessage()
                                    + ". Swap discovery and execution require Tor.",
                            false);
                }
            }
        });

        // Per-HttpUsage Tor circuit isolation for Whirlpool: see startWhirlpoolTorClients(),
        // started lazily by MixingService only when mixing is actually enabled/used.

        launch(args);
    }

    private static void maybeUpgradeTorFolder(File swapRootDir) {
        for (File file : swapRootDir.listFiles()) {
            if (file.getName().contains("tor") && file.isDirectory() && !file.getName().equals("tor-v" + TOR_BROWSER_VERSION)) {
                System.out.println("Upgrading Tor...");
                // Old Tor folder, transfer "hiddenservice" folder over, then delete
                File hiddenServiceDir = new File(file, "hiddenservice");
                File newTorFolder = new File(swapRootDir, "tor-v" + TOR_BROWSER_VERSION);
                if (!newTorFolder.exists()) newTorFolder.mkdir();
                File newHiddenServiceDir = new File(newTorFolder, "hiddenservice");
                hiddenServiceDir.renameTo(newHiddenServiceDir);
                deleteDir(file);
                break;
            }
        }
    }

    private static void deleteDir(File file) {
        File[] contents = file.listFiles();
        if (contents != null) {
            for (File f : contents) {
                if (!Files.isSymbolicLink(f.toPath())) {
                    deleteDir(f);
                }
            }
        }
        file.delete();
    }

    public static boolean isSwapRunning() {
        return appSwap != null && appSwap.getRunningSwap() && running.get();
    }

    public static boolean isAsbRunning() {
        return appAsb != null && appAsb.getRunningAsb() && !appAsb.getPeerId().isEmpty() && running.get();
    }

    public static void publishHiddenService() {
        // Creates directory named asb-onion in the directory that the above Tor client is located, and publishes hidden service to Tor network.
        // This onion address will be the ASB address for people to connect to.
        try {
            Tor tor = Tor.getDefault();
            if (tor != null) {
                hiddenServiceContainer = tor.publishHiddenService("asb-onion", AppAsb.ASB_PORT, AppAsb.ASB_PORT);
            }
        } catch (IOException | TorCtlException e) {
            e.printStackTrace();
        }
    }

    public static void applyTheme(Scene scene) {
        URL theme = GUISwap.class.getResource("/theme.css");
        if (theme != null) {
            scene.getStylesheets().add(theme.toExternalForm());
        }
    }

    @Override
    public void start(Stage stage) throws IOException {
        instance = this;
        Application.setUserAgentStylesheet(new PrimerDark().getUserAgentStylesheet());
//        Application.setUserAgentStylesheet(new CupertinoDark().getUserAgentStylesheet());
        FXMLLoader pairingFxmlLoader = new FXMLLoader(GUISwap.class.getResource("/pairing-screen.fxml"));
        Scene pairingScene = new Scene(pairingFxmlLoader.load(), 1240, 770);
        applyTheme(pairingScene);
//        Scene pairingScene = new Scene(pairingFxmlLoader.load(), Screen.getMainScreen().getWidth()/1.4f, Screen.getMainScreen().getHeight()/1.4f);
        pairingScene.setFill(Color.web("#16181d"));

        stage.getIcons().add(new Image("/images/outpost-icon.png"));
        stage.setTitle("Outpost v" + VERSION);
        stage.setScene(pairingScene);

        // Decide which layout the start screen shows (unlock, or connect a wallet) BEFORE the window
        // appears: otherwise it first shows everything the layout file holds, for an instant.
        ((PairingController) pairingFxmlLoader.getController()).init(stage);
        stage.show();

        stage.setOnCloseRequest(e -> {
            if (GUISwap.isSwapRunning() && !confirmCloseWithActiveSwap(stage)) {
                e.consume();
                return;
            }
            e.consume();
            stage.getScene().getRoot().setDisable(true);
            stage.setTitle("Shutting down...");
            requestShutdown();
        });

        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            requestShutdown();
            awaitShutdown();
        }));
    }

    @Override
    public void stop() {
        requestShutdown();
        awaitShutdown();
    }

    /**
     * Ask before closing while a swap is in flight. Losing the refund window is
     * irreversible (the counterparty can punish), so the safe action is the
     * default and Enter/Escape both keep the app open.
     */
    private static boolean confirmCloseWithActiveSwap(Stage stage) {
        SwapsController controller = SwapsController.instance;
        String summary = controller != null
                ? controller.activeSwapExitSummary()
                : "A swap is still in progress.";

        return PopupActiveSwapWarning.confirm(stage, summary, isAsbRunning());
    }

    private static void requestShutdown() {
        if (!shuttingDown.compareAndSet(false, true)) {
            return;
        }

        Thread shutdown = new Thread(GUISwap::performShutdown, "app-shutdown");
        shutdown.setDaemon(false);
        shutdown.start();
    }

    private static void awaitShutdown() {
        try {
            shutdownComplete.await(SHUTDOWN_WAIT_SECONDS, TimeUnit.SECONDS);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }

    private static void performShutdown() {
        try {
            GUISwap.running.set(false);
            GUISwap.scheduledExecutorService.shutdownNow();
            GUISwap.executorService.shutdownNow();

            AppSwap swapApp = appSwap;
            if (swapApp != null) swapApp.stop();

            AppAsb asbApp = appAsb;
            if (asbApp != null) asbApp.stop();

            AppPriceTicker priceTicker = appPriceTicker;
            if (priceTicker != null) priceTicker.stop();

            SwapsWallet wallet = SwapsWallet.getInstance();
            if (wallet != null) wallet.shutdown();

            MainController mainController = MainController.getInstance();
            if (mainController != null) mainController.stopTorConnCheckThread();

            shutdownTorBounded();
            HelperWallet.deleteAllFilesInFolder(new File(AppSwap.getSwapRootDir(), "tmp"), true);
        } catch (Throwable t) {
            log.error("Error during shutdown", t);
        } finally {
            shutdownComplete.countDown();
            Runtime.getRuntime().halt(0);
        }
    }

    /**
     * NativeTor.shutdown() can block forever in destroyHiddenService() when the
     * Tor process has already exited. Run it on a daemon thread and stop waiting
     * after a few seconds; the JVM halt below kills any straggler.
     */
    private static void shutdownTorBounded() {
        Thread torShutdown = new Thread(GUISwap::shutdownTor, "tor-shutdown");
        torShutdown.setDaemon(true);
        torShutdown.start();
        try {
            torShutdown.join(3_000);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
        if (torShutdown.isAlive()) {
            log.warn("Tor shutdown did not complete within 3s; continuing app shutdown");
        }
    }

    private static void shutdownTor() {
        Tor tor = Tor.getDefault();
        if (tor != null) {
            try {
                tor.shutdown();
            } catch (Exception e) {
                log.warn("Failed to shut down Tor: {}", e.getMessage());
            }
        }

        // The Whirlpool circuits are separate Tor instances; stopping the app
        // without them leaks tor processes across launches.
        for (Map.Entry<HttpUsage, NativeTor> entry : whirlpoolTorClients.entrySet()) {
            try {
                entry.getValue().shutdown();
            } catch (Exception e) {
                log.warn("Failed to shut down Whirlpool Tor for {}: {}", entry.getKey(), e.getMessage());
            }
        }
        whirlpoolTorClients.clear();
        whirlpoolTorStarted.set(false);
    }
}