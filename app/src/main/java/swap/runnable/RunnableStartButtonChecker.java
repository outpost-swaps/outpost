package swap.runnable;

import com.runjva.sourceforge.jsocks.protocol.Socks5Proxy;
import javafx.application.Platform;
import javafx.scene.control.Button;
import javafx.scene.control.Tooltip;
import javafx.scene.image.Image;
import javafx.scene.image.ImageView;
import org.berndpruenster.netlayer.tor.NativeTor;
import org.berndpruenster.netlayer.tor.Tor;
import org.berndpruenster.netlayer.tor.TorCtlException;
import swap.gui.GUISwap;
import swap.gui.controller.PairingController;
import swap.helper.HelperProperties;
import swap.model.ScreenType;
import swap.wallet.SwapsWallet;

import java.util.Collection;
import java.util.concurrent.atomic.AtomicBoolean;

public class RunnableStartButtonChecker implements Runnable {
    private final ScreenType screenType;
    private final ImageView imageView;
    private final Button startButton;
    private final Button stopButton;
    /** More buttons that must follow the same Tor gate as the start button (the first screen's Connect and Create). */
    private final java.util.List<Button> extraButtons;
    private boolean running = false;
    private boolean torHiddenServiceIsRunning = false;

    public RunnableStartButtonChecker(ScreenType screenType, ImageView imageView, Button startButton, Button stopButton) {
        this(screenType, imageView, startButton, stopButton, java.util.List.of());
    }

    public RunnableStartButtonChecker(ScreenType screenType, ImageView imageView, Button startButton, Button stopButton,
                                      java.util.List<Button> extraButtons) {
        this.screenType = screenType;
        this.imageView = imageView;
        this.startButton = startButton;
        this.stopButton = stopButton;
        this.extraButtons = extraButtons == null ? java.util.List.of() : extraButtons;
    }

    /**
     * Whether the buttons that begin pairing may be pressed. Pairing downloads the Monero wallet tools and opens the
     * wallet, both through Tor, and silently skips them when Tor is not up, so with Tor in use it waits for Tor.
     */
    static boolean pairingEnabled(boolean useTor, boolean torIsReady, boolean notCurrentlyPairing) {
        return useTor ? torIsReady && notCurrentlyPairing : notCurrentlyPairing;
    }

    @Override
    public void run() {
        running = true;
        while (running && !Thread.interrupted()) {
            boolean torClientIsRunning = false;
            try {
//                Collection<NativeTor> torClients = GUISwap.getInstance().getWhirlpoolTorClients().values();
//                boolean anyNotReady = torClients.stream().anyMatch(nativeTor -> {
//                    try {
//                        Socks5Proxy socks5Proxy = nativeTor.getProxy("127.0.0.1");
//                        return socks5Proxy.getPort() == 0 || !nativeTor.control.getRunning$tor();
//                    } catch (TorCtlException e) {
//                        return true;
//                    }
//                });
                Tor defaultTor = Tor.getDefault();
//                boolean defaultTorIsRunning = defaultTor != null && defaultTor.getProxy("127.0.0.1").getPort() != 0;
//                torClientIsRunning = !anyNotReady && defaultTorIsRunning && torClients.size() == HttpUsage.values().length;
                torClientIsRunning = defaultTor != null && defaultTor.getProxy("127.0.0.1").getPort() != 0;

                if (torClientIsRunning && GUISwap.hiddenServiceContainer == null) {
                    GUISwap.publishHiddenService();
                }

                torHiddenServiceIsRunning = GUISwap.hiddenServiceContainer != null;
            } catch (TorCtlException e) {
                throw new RuntimeException(e);
            }

            boolean enableStartButton = false;
            boolean torIsReady = torClientIsRunning && torHiddenServiceIsRunning;
            String torTooltip = torIsReady ? "Tor Connected" : "Tor Disconnected";

            updateGui(() -> {
                if (imageView != null) {
                    try {
                        String resource = torIsReady ? "/images/tor.png" : "/images/tor-offline.png";
                        java.net.URL url = RunnableStartButtonChecker.class.getResource(resource);
                        if (url != null) {
                            imageView.setImage(new Image(url.toExternalForm()));
                        }
                        Tooltip.install(imageView, new Tooltip(torTooltip));
                    } catch (Exception ignored) {
                    }
                }
            });

            switch (screenType) {
                case SWAPS -> {
//                    boolean whirlpoolReadySwapIdle = ClientWhirlpool.walletOpened.get() && !GUISwap.isSwapRunning() && !stopButton.isVisible();
                    if (HelperProperties.isUseTor()) {
                        enableStartButton = torIsReady;
                    }
//                    else {
//                        enableStartButton = whirlpoolReadySwapIdle;
//                    }
                }
                case PAIRING -> {
                    enableStartButton = pairingEnabled(HelperProperties.isUseTor(), torIsReady, !PairingController.pairing.get());
                }
                case WITHDRAW, UNKNOWN -> {
//                    boolean whirlpoolReady = ClientWhirlpool.walletOpened.get();
                    if (HelperProperties.isUseTor()) {
                        enableStartButton = torIsReady;
                    }
//                    else {
//                        enableStartButton = whirlpoolReady;
//                    }
                }
            }

            boolean finalEnableStartButton = enableStartButton;
            updateGui(() -> {
                for (Button extra : extraButtons) {
                    if (extra != null) extra.setDisable(!finalEnableStartButton);
                }
                if (startButton != null) {
                    startButton.setDisable(!finalEnableStartButton);

//                    boolean showStopButton = ClientWhirlpool.walletOpened.get() && GUISwap.isSwapRunning() && startButton.isDisabled();
                    boolean showStopButton = GUISwap.isSwapRunning() && startButton.isDisabled();
                    if(stopButton != null && !stopButton.isVisible() && showStopButton) {
                        stopButton.setDisable(false);
                        stopButton.setVisible(true);
                    }
                }
            });


            try {
                Thread.sleep(1000L);
            } catch (Exception ignored) {
            }
        }
    }

    public void shutdown() {
        running = false;
    }

    public void updateGui(Runnable runnable) {
        Platform.runLater(runnable);
    }
}
