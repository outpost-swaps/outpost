package swap.lib;

import org.berndpruenster.netlayer.tor.Tor;
import org.bitcoinj.core.Coin;
import org.bitcoinj.params.TestNet3Params;
import swap.gui.GUISwap;
import swap.gui.controller.MainController;
import swap.helper.HelperProperties;
import swap.model.LogType;
import swap.model.request.GetHistoryRequest;
import swap.model.request.StartAsbRequest;
import swap.model.response.GetHistoryResponse;
import swap.process.ProcessAsb;
import swap.process.ProcessRpcMonero;

import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.concurrent.atomic.AtomicBoolean;

public class AppAsb implements App {
    public static int ASB_PORT = 9939;
    private static Coin minQuantity;
    private static Coin maxQuantity;
    private static float fee;
    private static Coin manualPrice;
    private final AtomicBoolean runningAsb = new AtomicBoolean(false);
    private ProcessRpcMonero xmrRpcProcess = null;
    private ProcessAsb processAsb = null;
    private String peerId = "";
    private String externalAddress = "";
    private StartAsbRequest startAsbRequest;

    public static Coin getMinQuantity() {
        return minQuantity;
    }

    public static void setMinQuantity(Coin value) {
        minQuantity = value;
    }

    public static Coin getMaxQuantity() {
        return maxQuantity;
    }

    public static void setMaxQuantity(Coin value) {
        maxQuantity = value;
    }

    public static float getFee() {
        return fee;
    }

    public static void setFee(float value) {
        fee = value;
    }

    /** Fixed XMR price in BTC, or null when the ASB tracks the market. */
    public static Coin getManualPrice() {
        return manualPrice;
    }

    public static void setManualPrice(Coin value) {
        manualPrice = value;
    }

    public static File getAsbRootDir() {
        File dir = new File(AppSwap.getSwapRootDir() + "/asb");
        if (!dir.exists()) dir.mkdir();
        return dir;
    }

    @Override
    public void start() {
        setRunningAsb(false);
        AppSwap appSwap = GUISwap.appSwap;
        if (appSwap == null) return;
        if (Tor.getDefault() == null || appSwap.getProxyPort() == AppSwap.UNINITIALIZED_TOR_PORT) {
            throw new RuntimeException("Tor is required; refusing to start the ASB without a Tor proxy");
        }
        if (HelperProperties.previousPidAsb != null && !HelperProperties.previousPidAsb.isEmpty()) {
            ProcessAsb.shutdown(HelperProperties.previousPidAsb);
        }
        removeLegacySeedFile();
        this.xmrRpcProcess = null;
        String daemonAddress = appSwap.getMoneroDaemon();
        try {
            if (Tor.getDefault() != null) {
                String forwarderEndpoint = appSwap.startDaemonPoolForwarder();
                daemonAddress = forwarderEndpoint != null ? forwarderEndpoint : appSwap.getMoneroDaemon();
                int rpcTorPort = forwarderEndpoint != null ? AppSwap.UNINITIALIZED_TOR_PORT : appSwap.getProxyPort();
                this.xmrRpcProcess = AppXmrRpc.startMoneroRpc(appSwap.getParams(), appSwap.getRpcRootDir().getCanonicalPath(), daemonAddress, rpcTorPort, AppXmrRpc.RPC_ASB, forwarderEndpoint != null, appSwap.getMoneroDaemon());
                // The ASB opens its wallet against this endpoint right after
                // launch; connecting before the RPC server serves costs a full
                // 60s ASB restart-backoff cycle (observed: the port binds ~8s
                // before the server accepts). Probe instead of sleeping.
                if (!AppXmrRpc.waitUntilListening(this.xmrRpcProcess, 30000L)) {
                    System.out.println("[XMR_RPC] wallet RPC not accepting connections after 30s; starting ASB anyway");
                }
            }
        } catch (IOException e) {
            throw new RuntimeException(e);
        }

        // Prefer handing the ASB the real endpoint pool: its own balancer
        // fails over and cools down hung sessions, and it survives the local
        // forwarder process dying (observed: ASB pinned to a dead port).
        List<String> directElectrumUrls = appSwap.getDirectElectrumPoolUrls();
        System.out.println("[ASB] electrum pool urls=" + directElectrumUrls
                + " saved=" + appSwap.getElectrumServer()
                + " failover=" + swap.helper.HelperProperties.isElectrumFailoverInEffect()
                + " curated=" + appSwap.getElectrumServers().size());
        String asbElectrumUrl = directElectrumUrls.isEmpty()
                ? appSwap.getSafeElectrumEndpoint(appSwap.getElectrumServer())
                : directElectrumUrls.get(0);
        StartAsbRequest startAsbRequest = new StartAsbRequest(
                asbElectrumUrl,
                directElectrumUrls,
                appSwap.getProxyAddress(),
                appSwap.getProxyPort(),
                getXmrRpcProcess().endpoint(),
                daemonAddress,
                appSwap.getRendezvousPeers(),
                false,
                Float.parseFloat(minQuantity.toPlainString()),
                Float.parseFloat(maxQuantity.toPlainString()),
                fee,
                getManualPrice(),
                appSwap.getParams(),
                getHiddenServiceOnion().replace(".onion", "")
        );
        GUISwap.executorService.submit(() -> {
            try {
                setRunningAsb(true);
                startAsb(startAsbRequest);
            } catch (Throwable failure) {
                swap.process.ProcessAsb.restartWithBackoff("ASB");
            }
        });
    }

    @Override
    public void stop() {
        setRunningAsb(false);
        MainController mainController = MainController.getInstance();
        if (mainController != null) {
            mainController.printSwapLogLn(LogType.HIGHLIGHT, "[ASB] Running ASB shutdown process...", false);
        }

        // Mixing is tied to the ASB's lifecycle - stop it whenever the ASB stops, not just
        // on full app shutdown, regardless of what happens below.
        swap.mixing.MixingService.disable();

        ProcessAsb processAsb = getAsbProcess();
        if (processAsb != null) {
            boolean successfulAsbShutdown = processAsb.shutdown();
            if (successfulAsbShutdown) this.processAsb = null;
        }

        ProcessRpcMonero processRpcMonero = getXmrRpcProcess();
        if (processRpcMonero == null) return;
        boolean successfulRpcShutdown = processRpcMonero.shutdown();
        if (successfulRpcShutdown) this.xmrRpcProcess = null;
    }

    @Override
    public void restart() {
        this.stop();
        this.start();
    }

    private ProcessRpcMonero getXmrRpcProcess() {
        return xmrRpcProcess;
    }

    /**
     * Endpoint of the ASB's monero-wallet-rpc instance, or null when the ASB
     * (and its wallet RPC) is not running. Used by the settings page to read
     * the asb-wallet seed via query_key while the wallet is open.
     */
    public String getXmrRpcEndpoint() {
        return xmrRpcProcess != null ? xmrRpcProcess.endpoint() : null;
    }

    private ProcessAsb getAsbProcess() {
        return processAsb;
    }

    public String getHiddenServiceOnion() {
        return GUISwap.hiddenServiceContainer.getHostname();
    }

    public void startAsb(StartAsbRequest startAsbRequest) {
        try {
            this.startAsbRequest = startAsbRequest;
            String configFilePath = createConfigFile(startAsbRequest);
            String executablePath = copyBinaryFile();
            launchProcess(startAsbRequest, executablePath, configFilePath);
        } catch (Exception e) {
            // start() sets the running flag before reaching here; a failed launch must
            // clear it and surface the error, or the UI would show a "running" ASB that
            // does not exist.
            setRunningAsb(false);
            MainController mainController = MainController.getInstance();
            if (mainController != null) {
                mainController.printSwapLogLn(LogType.ERROR, "[ASB] Failed to start: " + e.getMessage(), false);
            }
        }
    }

    private void removeLegacySeedFile() {
        AppSwap appSwap = GUISwap.appSwap;
        if (appSwap == null) return;
        for (String network : List.of("mainnet", "testnet")) {
            File seedPemFile = new File(new File(AppAsb.getAsbRootDir(), network), "seed.pem");
            if (seedPemFile.exists() && !seedPemFile.delete()) {
                throw new IllegalStateException("Could not remove legacy plaintext ASB seed file: " + seedPemFile);
            }
        }
    }

    private void launchProcess(StartAsbRequest startAsbRequest, String executablePath, String configFilePath) {
        if (!startAsbRequest.params().equals(GUISwap.appSwap.getParams())) {
            throw new IllegalArgumentException("ASB network must match the paired wallet network");
        }
        // The Dojo API key goes over stdin with the seed, not in the environment (audit M-6).
        byte[] seedPayload = stdinPayload(GUISwap.appSwap.getSeedAsBase64(), HelperProperties.dojoApiKey);
        ArrayList<String> cmd = new ArrayList<>();
        cmd.add(executablePath);
        if (startAsbRequest.params() == TestNet3Params.get()) {
            cmd.add("--testnet");
        }
        cmd.add("--json");
        cmd.add("--config");
        cmd.add(configFilePath);
        cmd.add("--seed-stdin");
        cmd.add("start");

        try {
            ProcessBuilder processBuilder = new ProcessBuilder(cmd);
            applyDojoEnvironment(processBuilder);
            Process process = processBuilder.start();
            writeSeedToProcess(process, seedPayload);
            HelperProperties.setProperty(HelperProperties.KEY_PID_ASB, process.pid() + "");
            this.processAsb = new ProcessAsb(process);
            MainController.getInstance().printSwapLogLn(LogType.INFO, "[ASB] Initializing...", false);
        } catch (IOException e) {
            throw new RuntimeException(e);
        }
    }

    static void applyDojoEnvironment(ProcessBuilder processBuilder) {
        boolean hasUrl = HelperProperties.dojoUrl != null && !HelperProperties.dojoUrl.isBlank();
        boolean hasKey = HelperProperties.dojoApiKey != null && !HelperProperties.dojoApiKey.isBlank();
        if (!hasUrl || !hasKey) {
            throw new IllegalStateException(
                    "Incomplete Dojo fee configuration: URL and API key must both be available");
        }
        processBuilder.environment().put("OUTPOST_DOJO_URL", HelperProperties.dojoUrl);
        // The key is a secret: other processes of the same user can read an environment, so it
        // travels over stdin with the seed instead (stdinPayload). Never inherit a stray one.
        processBuilder.environment().remove("OUTPOST_DOJO_API_KEY");
    }

    /** The ASB's stdin: the seed on the first line, the Dojo API key on the second. */
    static byte[] stdinPayload(String seedBase64, String dojoApiKey) {
        byte[] seed = seedStdinPayload(seedBase64);
        if (dojoApiKey == null || dojoApiKey.isBlank()) {
            return seed;
        }
        if (dojoApiKey.contains("\n") || dojoApiKey.contains("\r")) {
            throw new IllegalStateException("The Dojo API key cannot contain a line break.");
        }
        return (new String(seed, StandardCharsets.US_ASCII) + "\n" + dojoApiKey.trim())
                .getBytes(StandardCharsets.UTF_8);
    }

    static byte[] seedStdinPayload(String seedBase64) {
        if (seedBase64 == null || seedBase64.isBlank()) {
            throw new IllegalStateException("Paired wallet seed is not available to start the ASB.");
        }
        return seedBase64.getBytes(StandardCharsets.US_ASCII);
    }

    static void writeSeedToProcess(Process process, byte[] seedPayload) throws IOException {
        try (OutputStream stdin = process.getOutputStream()) {
            stdin.write(seedPayload);
        } catch (IOException e) {
            process.destroy();
            throw e;
        }
    }

    private String createConfigFile(StartAsbRequest startAsbRequest) {
        File configFile = new File(AppAsb.getAsbRootDir(), "config.toml");
        if (configFile.exists()) configFile.delete();
        // will go into outpost-swaps/asb/config.toml
        HashMap<String, List<String>> tomlProperties = startAsbRequest.toConfigToml();
        StringBuilder stringBuilder = new StringBuilder();
        for (String category : tomlProperties.keySet()) {
            stringBuilder.append("[").append(category).append("]").append("\n");
            for (String property : tomlProperties.get(category)) {
                stringBuilder.append(property).append("\n");
            }
            stringBuilder.append("\n");
        }

        try {
            writeOwnerOnly(configFile.toPath(), stringBuilder.toString());
            return configFile.getAbsolutePath();
        } catch (IOException e) {
            throw new RuntimeException(e);
        }
    }

    /**
     * Writes a file that only the owner can read, created that way so there is no moment with
     * default permissions. The ASB config holds the wallet RPC login.
     */
    static void writeOwnerOnly(java.nio.file.Path file, String content) throws IOException {
        java.nio.file.Files.deleteIfExists(file);
        try {
            java.nio.file.Files.createFile(
                    file,
                    java.nio.file.attribute.PosixFilePermissions.asFileAttribute(
                            java.nio.file.attribute.PosixFilePermissions.fromString("rw-------")));
        } catch (UnsupportedOperationException e) {
            java.nio.file.Files.createFile(file); // no POSIX modes (Windows)
        }
        java.nio.file.Files.writeString(file, content);
    }

    private String copyBinaryFile() throws Exception {
        // Refuse to start the ASB from a library/asb pair built at different times
        // (swap 7207846b: they derived fees differently).
        swap.helper.HelperBuildManifest.verifyBundledOnce();
        boolean windows = System.getProperty("os.name").toLowerCase().contains("windows");
        String resourceName = windows ? "/binaries/asb.exe" : "/binaries/asb";
        File bin = getResourceAsFile(resourceName);
        if (bin == null || !bin.exists()) {
            throw new IOException("ASB executable resource is missing: " + resourceName);
        }

        File asbBinDir = new File(getAsbRootDir(), "bin");
        if (!asbBinDir.exists() && !asbBinDir.mkdirs()) {
            throw new IOException("Could not create ASB binary directory: " + asbBinDir);
        }
        String destinationName = windows ? "asb.exe" : "asb";
        Path destination = Path.of(asbBinDir.getPath(), destinationName);
        Path result = Files.copy(bin.toPath(), destination, StandardCopyOption.REPLACE_EXISTING);
        setAsbExecutablePerms(result.toFile());
        return result.toString();
    }

    private void setAsbExecutablePerms(File file) {
        file.setReadable(true);
        file.setExecutable(true);
        file.setWritable(false);
        file.setWritable(true, true);
    }

    public String getPeerId() {
        return peerId;
    }

    public void setPeerId(String peerId) {
        this.peerId = peerId;
    }

    public String getExternalAddress() {
        return externalAddress;
    }

    public void setExternalAddress(String externalAddress) {
        this.externalAddress = externalAddress;
    }

    public boolean getRunningAsb() {
        return runningAsb.get();
    }

    public void setRunningAsb(boolean status) {
        this.runningAsb.set(status);
        MainController controller = MainController.getInstance();
        if (controller != null) controller.setAsbRunning(status);
    }

    public StartAsbRequest getStartAsbRequest() {
        return startAsbRequest;
    }

    public GetHistoryResponse getHistory(GetHistoryRequest getHistoryRequest) {
        String seedBase64 = GUISwap.appSwap == null ? "" : GUISwap.appSwap.getSeedAsBase64();
        String json = getHistory(getHistoryRequest.toJson()
                .put("seedBase64", seedBase64)
                .toString());
        if (json == null) {
            System.err.println("ASB history request returned no response; showing empty history.");
            return GetHistoryResponse.empty();
        }
        return GetHistoryResponse.fromJson(json);
    }

    private native String getHistory(String json);

    private File getResourceAsFile(String resourcePath) {
        try {
            InputStream in = getClass().getResourceAsStream(resourcePath);
            if (in == null) {
                return null;
            }

            File tempFile = File.createTempFile(String.valueOf(in.hashCode()), ".tmp");
            tempFile.deleteOnExit();

            try (FileOutputStream out = new FileOutputStream(tempFile)) {
                //copy stream
                byte[] buffer = new byte[1024];
                int bytesRead;
                while ((bytesRead = in.read(buffer)) != -1) {
                    out.write(buffer, 0, bytesRead);
                }
            }
            return tempFile;
        } catch (IOException e) {
            e.printStackTrace();
            return null;
        }
    }
}
