package swap.helper;

import com.samourai.wallet.crypto.AESUtil;
import com.samourai.wallet.crypto.DecryptionException;
import com.samourai.wallet.util.CharSequenceX;
import org.bitcoinj.core.Coin;
import org.bouncycastle.crypto.InvalidCipherTextException;
import swap.gui.GUISwap;
import swap.lib.AppAsb;
import swap.lib.AppSwap;
import swap.lib.AppXmrRpc;
import swap.model.SwapsPairing;

import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.io.UnsupportedEncodingException;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.FileAlreadyExistsException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.nio.file.StandardOpenOption;
import java.nio.file.attribute.PosixFilePermissions;
import java.util.Properties;

public class HelperProperties {
    public static final String KEY_PID_ASB = "asb_pid";
    private static final String KEY_SEED = "cli.seed";
    private static final String KEY_PASSPHRASE = "cli.seedAppendPassphrase";
    private static final String KEY_NETWORK = "cli.server";
    private static final String KEY_TOR = "cli.tor";
    private static final String KEY_APIKEY = "cli.apiKey";
    private static final String KEY_DOJO_ENABLED = "cli.dojo.enabled";
    private static final String KEY_DOJO_URL = "cli.dojo.url";
    private static final String KEY_DOJO_API_KEY = "cli.dojo.apiKey";
    private static final String KEY_DOJO_API_KEY_ENCRYPTED = "cli.dojo.apiKeyEncrypted";

    private static final String KEY_MIN_QUANTITY = "minQuantity";
    private static final String KEY_MAX_QUANTITY = "maxQuantity";
    private static final String KEY_FEE = "fee";
    private static final String KEY_MANUAL_RATE = "manualRate";
    public static final String KEY_MONERO_DAEMON = "moneroDaemon";
    public static final String KEY_ELECTRUM_SERVER = "electrumServer";
    private static final String KEY_PID_RPC_ASB = AppXmrRpc.RPC_ASB + "_pid";
    private static final String KEY_PID_RPC_SWAPCLIENT = AppXmrRpc.RPC_SWAPCLIENT + "_pid";
    private static final String KEY_AUTO_TX0 = "autoTx0";
    private static final String KEY_POOL_SIZE = "poolSize";
    private static final String KEY_SCODE = "scode";
    private static final String KEY_TRUST_REMOTE_DAEMON = "trustRemoteDaemon";
    private static final String KEY_MONERO_NODE_IS_OWN = "moneroNodeIsOwn";
    private static final String KEY_DAEMON_FAILOVER = "daemonFailover";
    private static final String KEY_ELECTRUM_FAILOVER = "electrumFailover";
    private static final String KEY_ELECTRUM_SERVER_IS_OWN = "electrumServerIsOwn";
    private static final String KEY_FILE_VERSION = "properties.version";
    public static final String KEY_HAS_SEEN_UPDATE_POPUP = "popup.seen." + GUISwap.VERSION;

    public static String mnemonicEncrypted = null;
    public static String network = null;
    public static boolean hasPassphrase = false;
    public static String apiKey = null;
    public static String minQuantity = null;
    public static String maxQuantity = null;
    public static String fee = null;
    public static String manualRate = null; // BTC per 1 XMR; null/empty = off
    public static String moneroDaemon = null; // set in SwapClient
    public static String electrumServer = null;
    public static String previousPidRpcSwapClient = null;
    public static String previousPidRpcAsb = null;
    public static String previousPidAsb = null;
    public static int propertiesVersion = 1;
    // Tor is mandatory: there is no clearnet configuration path.
    private static final boolean useTor = true;
    private static boolean autoTx0 = false;
    private static String poolSize = null;
    private static String scode = null;
    public static String dojoUrl = null;
    public static String dojoApiKey = null;
    public static boolean hasSeenUpdatePopup = false;

    public static void init(String pairingPayload) throws Exception {
        Properties appProperties = new Properties();
        if (hasPropertiesFile()) {
            File propertiesFile = getPropertiesFile();
            try (FileInputStream input = new FileInputStream(propertiesFile)) {
                appProperties.load(input);
            }
            restrictToOwner(propertiesFile.toPath());
            mnemonicEncrypted = appProperties.getProperty(KEY_SEED);
            network = appProperties.getProperty(KEY_NETWORK).toUpperCase();
            // Ignore any legacy KEY_TOR=false value; Tor is mandatory.
            hasPassphrase = Boolean.parseBoolean(appProperties.getProperty(KEY_PASSPHRASE));
            apiKey = appProperties.getProperty(KEY_APIKEY, SwapsPairing.generateApiKey()); // TODO:
            dojoUrl = appProperties.getProperty(KEY_DOJO_URL, null);
            dojoApiKey = appProperties.getProperty(KEY_DOJO_API_KEY, null);
            minQuantity = appProperties.getProperty(KEY_MIN_QUANTITY, null);
            maxQuantity = appProperties.getProperty(KEY_MAX_QUANTITY, null);
            manualRate = appProperties.getProperty(KEY_MANUAL_RATE, "");
            previousPidAsb = appProperties.getProperty(KEY_PID_ASB);
            previousPidRpcSwapClient = appProperties.getProperty(KEY_PID_RPC_SWAPCLIENT);
            previousPidRpcAsb = appProperties.getProperty(KEY_PID_RPC_ASB);
            fee = appProperties.getProperty(KEY_FEE);
            moneroDaemon = appProperties.getProperty(KEY_MONERO_DAEMON);
            electrumServer = appProperties.getProperty(KEY_ELECTRUM_SERVER);
            autoTx0 = Boolean.parseBoolean(appProperties.getProperty(KEY_AUTO_TX0, "false"));
            poolSize = appProperties.getProperty(KEY_POOL_SIZE, "0.01btc");
            scode = appProperties.getProperty(KEY_SCODE);
            propertiesVersion = Integer.parseInt(appProperties.getProperty(KEY_FILE_VERSION, "1"));
            hasSeenUpdatePopup = Boolean.parseBoolean(appProperties.getProperty(KEY_HAS_SEEN_UPDATE_POPUP, "false"));

            migrateDeadSavedEndpoints(appProperties);

            if (mnemonicEncrypted.isEmpty() || network.isEmpty()) {
                throw new RuntimeException("Failed to load properties");
            }

            // Re-parse pairing payload if Dojo info is missing from the properties file
            // and the user supplied a pairing payload this session. This handles users who
            // paired before the dojo.pairing.url JSON-path bug was fixed in SwapsPairing.
            boolean dojoMissing = (dojoUrl == null || dojoUrl.isEmpty())
                    || !hasDojoApiKey(appProperties);
            if (dojoMissing && pairingPayload != null && !pairingPayload.isEmpty()) {
                try {
                    SwapsPairing reparsed = new SwapsPairing(pairingPayload);
                    String newDojoUrl = reparsed.getDojoUrl();
                    String newDojoApiKey = reparsed.getDojoApiKey();
                    if (newDojoUrl != null && !newDojoUrl.isEmpty()
                            && newDojoApiKey != null && !newDojoApiKey.isEmpty()) {
                        dojoUrl = newDojoUrl;
                        dojoApiKey = newDojoApiKey;
                        appProperties.setProperty(KEY_DOJO_URL, dojoUrl);
                        // The key stays in memory until unlockSecrets encrypts it;
                        // it is not stored in plaintext.
                        appProperties.setProperty(KEY_DOJO_ENABLED, "true");
                    }
                } catch (Exception ignored) {
                    // Invalid re-paste — keep existing (missing) dojo state
                }
            }
        } else {
            if (pairingPayload == null) {
                throw new RuntimeException("Failed to load properties");
            } else {
                SwapsPairing samouraiPairing = new SwapsPairing(pairingPayload); // TODO
                mnemonicEncrypted = samouraiPairing.getEncryptedSeed();
                network = samouraiPairing.getNetwork();
                hasPassphrase = samouraiPairing.hasPassphrase();
                apiKey = samouraiPairing.getApiKey();
                dojoUrl = samouraiPairing.getDojoUrl();
                dojoApiKey = samouraiPairing.getDojoApiKey();
                minQuantity = "0.001"; // defaults
                maxQuantity = "0.05";
                fee = "5";
                poolSize = "0.01btc";
                propertiesVersion = 1;
                appProperties.setProperty(KEY_SEED, mnemonicEncrypted);
                appProperties.setProperty(KEY_PASSPHRASE, String.valueOf(hasPassphrase));
                appProperties.setProperty(KEY_NETWORK, network);
                appProperties.setProperty(KEY_APIKEY, apiKey);

                if (dojoUrl != null && dojoApiKey != null) {
                    appProperties.setProperty(KEY_DOJO_URL, dojoUrl);
                    // Memory only until unlockSecrets encrypts it with the passphrase.
                    appProperties.setProperty(KEY_DOJO_ENABLED, String.valueOf(true));
                } else {
                    appProperties.setProperty(KEY_DOJO_ENABLED, String.valueOf(false));
                }

                appProperties.setProperty(KEY_TOR, String.valueOf(useTor));
                appProperties.setProperty(KEY_MIN_QUANTITY, minQuantity);
                appProperties.setProperty(KEY_MAX_QUANTITY, maxQuantity);
                appProperties.setProperty(KEY_FEE, fee);
                appProperties.setProperty(KEY_AUTO_TX0, String.valueOf(autoTx0));
                appProperties.setProperty(KEY_POOL_SIZE, poolSize);
                appProperties.setProperty(KEY_FILE_VERSION, String.valueOf(propertiesVersion));
                AppSwap.getSwapRootDir();
            }
        }

        double min = Double.parseDouble(minQuantity);
        if (min < 0.0005) {
            throw new RuntimeException("It is not recommended to set min quantity below 0.0005 BTC");
        }

        AppAsb.setMinQuantity(Coin.parseCoin(minQuantity));
        AppAsb.setMaxQuantity(Coin.parseCoin(maxQuantity));
        AppAsb.setFee(Float.parseFloat(fee));
        AppAsb.setManualPrice(manualRate == null || manualRate.isBlank() ? null : Coin.parseCoin(manualRate));

        store(appProperties, "store atomic-swaps.properties");
    }

    public static void unlockSecrets(String passphrase) throws IOException, InvalidCipherTextException,
            DecryptionException, UnsupportedEncodingException {
        synchronized (PROPERTIES_LOCK) {
            Properties properties = new Properties();
            try (FileInputStream input = new FileInputStream(getPropertiesFile())) {
                properties.load(input);
            }
            if (migrateDojoApiKey(properties, passphrase)) {
                store(properties, "store atomic-swaps.properties");
            }
        }
    }

    /**
     * Replaces the Dojo in a settings file: the URL, enabled, and the API key encrypted with the wallet
     * passphrase (never stored in plaintext; any plaintext key is removed).
     */
    static void applyDojo(Properties properties, String url, String apiKey, String passphrase) {
        try {
            properties.setProperty(KEY_DOJO_URL, url);
            properties.setProperty(KEY_DOJO_ENABLED, "true");
            properties.setProperty(KEY_DOJO_API_KEY_ENCRYPTED, AESUtil.encrypt(apiKey, new CharSequenceX(passphrase)));
            properties.remove(KEY_DOJO_API_KEY);
        } catch (DecryptionException | UnsupportedEncodingException e) {
            throw new IllegalStateException("Could not encrypt the Dojo API key", e);
        }
    }

    /**
     * Saves a different Dojo. The running app keeps using the old one until it is restarted (the wallet
     * and the fee estimator set the Dojo up at start), so the whole app switches together.
     */
    public static void saveDojo(String url, String apiKey, String passphrase) throws IOException {
        update(props -> applyDojo(props, url, apiKey, passphrase));
    }

    static boolean migrateDojoApiKey(Properties properties, String passphrase)
            throws DecryptionException, UnsupportedEncodingException, InvalidCipherTextException {
        String encrypted = properties.getProperty(KEY_DOJO_API_KEY_ENCRYPTED);
        String plaintext = properties.getProperty(KEY_DOJO_API_KEY);
        CharSequenceX secret = new CharSequenceX(passphrase == null ? "" : passphrase);

        if (encrypted != null && !encrypted.isEmpty()) {
            dojoApiKey = AESUtil.decrypt(encrypted, secret);
            if (plaintext != null) {
                properties.remove(KEY_DOJO_API_KEY);
                return true;
            }
            return false;
        }

        if (plaintext == null || plaintext.isEmpty()) {
            // Freshly paired: the key has only been held in memory so far (it is
            // never written to disk in plaintext). Encrypt and store it now that
            // the passphrase is available.
            if (dojoApiKey == null || dojoApiKey.isEmpty()) {
                return false;
            }
            properties.setProperty(KEY_DOJO_API_KEY_ENCRYPTED, AESUtil.encrypt(dojoApiKey, secret));
            return true;
        }

        String ciphertext = AESUtil.encrypt(plaintext, secret);
        properties.remove(KEY_DOJO_API_KEY);
        properties.setProperty(KEY_DOJO_API_KEY_ENCRYPTED, ciphertext);
        dojoApiKey = plaintext;
        return true;
    }

    private static boolean hasDojoApiKey(Properties properties) {
        String encrypted = properties.getProperty(KEY_DOJO_API_KEY_ENCRYPTED);
        String plaintext = properties.getProperty(KEY_DOJO_API_KEY);
        return (encrypted != null && !encrypted.isEmpty())
                || (plaintext != null && !plaintext.isEmpty());
    }

    /**
     * Clears saved electrum/monero picks that point at known-dead endpoints so
     * AppSwap.setupConfig re-picks from the current defaults and saves those
     * (without this, a dead saved pick overrides fixed defaults forever).
     */
    private static void migrateDeadSavedEndpoints(Properties appProperties) {
        if (HelperDeadEndpoints.isDead(electrumServer)) {
            appProperties.remove(KEY_ELECTRUM_SERVER);
            electrumServer = null;
        }
        if (HelperDeadEndpoints.isDead(moneroDaemon)) {
            appProperties.remove(KEY_MONERO_DAEMON);
            moneroDaemon = null;
        }
    }

    public static File getPropertiesFile() {
        return new File(AppSwap.getSwapRootDir(), "atomic-swaps.properties");
    }

    public static boolean hasPropertiesFile() {
        return getPropertiesFile().exists();
    }

    public static void setProperty(String key, String value) throws IOException {
        update(getPropertiesFile().toPath(), props -> props.setProperty(key, value));
    }

    /**
     * One lock for every read-modify-write of the properties file: without it two threads changing
     * different keys overwrite each other and a seed or Dojo key written a moment earlier is lost.
     */
    private static final Object PROPERTIES_LOCK = new Object();

    /** Loads the properties file, lets {@code change} edit it and saves it again, all under the lock. */
    public static void update(java.util.function.Consumer<Properties> change) throws IOException {
        update(getPropertiesFile().toPath(), change);
    }

    static void update(Path target, java.util.function.Consumer<Properties> change) throws IOException {
        synchronized (PROPERTIES_LOCK) {
            Properties props = new Properties();
            try (java.io.InputStream input = Files.newInputStream(target)) {
                props.load(input);
            }
            change.accept(props);
            storeAtomically(props, "store atomic-swaps.properties", target);
        }
    }

    /**
     * Writes the properties through a same-directory temporary file so a crash
     * cannot truncate the seed/Dojo configuration, and keeps the file readable
     * only by its owner. The properties include the encrypted seed and the
     * passphrase-encrypted Dojo API key.
     */
    public static void store(Properties properties, String comment) throws IOException {
        storeAtomically(properties, comment, getPropertiesFile().toPath());
    }

    static void storeAtomically(Properties properties, String comment, Path target) throws IOException {
        synchronized (PROPERTIES_LOCK) {
            storeAtomicallyLocked(properties, comment, target);
        }
    }

    private static void storeAtomicallyLocked(Properties properties, String comment, Path target) throws IOException {
        Path absoluteTarget = target.toAbsolutePath();
        Path parent = absoluteTarget.getParent();
        if (parent == null) {
            throw new IOException("Properties file has no parent directory: " + target);
        }
        Files.createDirectories(parent);

        Path temp = Files.createTempFile(parent, ".atomic-swaps-", ".tmp");
        try {
            try (OutputStream output = Files.newOutputStream(
                    temp, StandardOpenOption.WRITE, StandardOpenOption.TRUNCATE_EXISTING)) {
                properties.store(output, comment);
            }
            restrictToOwner(temp);

            try {
                Files.move(
                        temp,
                        absoluteTarget,
                        StandardCopyOption.REPLACE_EXISTING,
                        StandardCopyOption.ATOMIC_MOVE);
            } catch (AtomicMoveNotSupportedException | FileAlreadyExistsException e) {
                Files.move(temp, absoluteTarget, StandardCopyOption.REPLACE_EXISTING);
            }
            restrictToOwner(absoluteTarget);
        } finally {
            Files.deleteIfExists(temp);
        }
    }

    static void restrictToOwner(Path path) {
        try {
            Files.setPosixFilePermissions(path, PosixFilePermissions.fromString("rw-------"));
        } catch (UnsupportedOperationException | IOException ignored) {
            // Windows and non-POSIX filesystems do not expose Unix permissions.
        }
    }

    /** Makes a directory unlistable by other users (matrix D-02). No-op where POSIX modes do not exist. */
    /**
     * The swap database is a file named "sqlite" in the data folder. The engine's buy-swap code opens it
     * without creating it, so on a new profile the first swap failed at once with "unable to open database
     * file". An empty file is a valid new SQLite database: the engine fills it in on first use. Never touches
     * an existing file.
     */
    public static void ensureSwapDatabaseFile(Path dataDir) {
        Path database = dataDir.resolve("sqlite");
        try {
            if (Files.exists(database)) {
                return;
            }
            Files.createFile(database);
            try {
                Files.setPosixFilePermissions(database, PosixFilePermissions.fromString("rw-------"));
            } catch (UnsupportedOperationException ignored) {
                // Windows and non-POSIX filesystems do not expose Unix permissions.
            }
        } catch (IOException e) {
            System.out.println("[DATA] could not create the swap database file: " + e.getMessage());
        }
    }

    public static void restrictDirectoryToOwner(Path dir) {
        try {
            Files.setPosixFilePermissions(dir, PosixFilePermissions.fromString("rwx------"));
        } catch (UnsupportedOperationException | IOException ignored) {
            // Windows and non-POSIX filesystems do not expose Unix permissions.
        }
    }

    public static String getProperty(String key) {
        Properties props = new Properties();
        try (FileInputStream input = new FileInputStream(getPropertiesFile())) {
            props.load(input);
        } catch (IOException e) {
            return null;
        }
        return props.getProperty(key);
    }

    public static boolean isUseTor() {
        return useTor;
    }

    public static boolean isAutoTx0() {
        return autoTx0;
    }

    public static String getPoolSize() {
        return poolSize;
    }

    public static String getScode() {
        return scode;
    }

    public static void setAutoTx0(boolean value) throws IOException {
        autoTx0 = value;
        setProperty(KEY_AUTO_TX0, String.valueOf(value));
    }

    public static void setPoolSize(String value) throws IOException {
        poolSize = value;
        setProperty(KEY_POOL_SIZE, value);
    }

    public static void setScode(String value) throws IOException {
        scode = value;
        setProperty(KEY_SCODE, value == null ? "" : value);
    }

    /**
     * Whether the user declared the saved Monero node to be their own. A swap then trusts it alone
     * for the check that Alice's lock exists (when failover is off); every other node needs
     * witnesses. This is not "Trust Daemon", which only enables bigger sync batches for speed.
     */
    public static boolean isMoneroNodeOwn() {
        String raw = getProperty(KEY_MONERO_NODE_IS_OWN);
        return raw != null && raw.equals("true");
    }

    /**
     * Failover as it actually applies: a node declared as the user's own is pinned, so failover is
     * off whatever the saved failover switch says (it would otherwise add public nodes that see
     * the user's sync pattern).
     */
    public static boolean isDaemonFailoverInEffect() {
        return !isMoneroNodeOwn() && isDaemonFailover();
    }

    /** Trusted-daemon sync as it applies: the user's own node is always trusted. */
    public static boolean isTrustDaemonInEffect() {
        return isMoneroNodeOwn() || isTrustRemoteDaemon();
    }

    /**
     * Whether the user has opted to trust REMOTE daemons on MAINNET (faster
     * wallet sync: larger block batches, fewer round-trips). The test networks
     * and any localhost daemon are always trusted regardless of this setting.
     */
    public static boolean isTrustRemoteDaemon() {
        String raw = getProperty(KEY_TRUST_REMOTE_DAEMON);
        return raw != null && raw.equals("true");
    }

    /**
     * Whether the daemon-pool forwarder may switch to another curated
     * daemon when the active one drops. This switch is authoritative: off
     * means the configured daemon is PINNED, curated default or not.
     * Enabling it also opts a user-configured daemon into the curated
     * fallbacks. Opt-in: unset means OFF (pinned), so a user's daemon is never
     * swapped for, or joined by, other daemons unless they turned this on.
     */
    public static boolean isDaemonFailover() {
        return "true".equals(getProperty(KEY_DAEMON_FAILOVER));
    }

    /**
     * Whether the electrum-pool forwarder may switch to another curated
     * server when the active one drops. This switch is authoritative: off
     * means the configured server is PINNED, curated default or not.
     * Enabling it also opts a user-configured server into the curated
     * fallbacks. Independent of the daemon failover switch. Opt-in: unset
     * means OFF, so a user's server is never swapped for, or joined by,
     * other servers (which would see the wallet's addresses) unless they
     * turned this on.
     */
    public static boolean isElectrumFailover() {
        return "true".equals(getProperty(KEY_ELECTRUM_FAILOVER));
    }

    /** Whether the user declared the selected Electrum server to be their own. */
    public static boolean isElectrumServerOwn() {
        return "true".equals(getProperty(KEY_ELECTRUM_SERVER_IS_OWN));
    }

    /**
     * Electrum failover as it applies: the user's own server is pinned, whatever the saved
     * failover switch says, because a fallback would show every wallet address to a public server.
     */
    public static boolean isElectrumFailoverInEffect() {
        return !isElectrumServerOwn() && isElectrumFailover();
    }

    /**
     * Fixed XMR price in BTC advertised by the ASB when set (override:
     * the market ticker is bypassed, no spread applied); null/empty when
     * the ASB tracks the market.
     */
    public static String getManualRate() {
        return getProperty(KEY_MANUAL_RATE);
    }

    public static void setManualRate(String value) throws IOException {
        manualRate = value == null ? "" : value;
        setProperty(KEY_MANUAL_RATE, manualRate);
    }
}
