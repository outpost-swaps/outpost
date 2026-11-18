package swap.lib;

import com.samourai.wallet.api.backend.BackendApi;
import com.samourai.wallet.crypto.DecryptionException;
import com.samourai.wallet.util.FormatsUtilGeneric;
import com.samourai.whirlpool.client.wallet.data.wallet.WalletSupplierImpl;
import javafx.application.Platform;
import org.berndpruenster.netlayer.tor.NativeTor;
import org.berndpruenster.netlayer.tor.Tor;
import org.berndpruenster.netlayer.tor.TorCtlException;
import org.bitcoinj.core.NetworkParameters;
import org.bitcoinj.core.Sha256Hash;
import org.bitcoinj.crypto.MnemonicException;
import org.bitcoinj.params.MainNetParams;
import org.bitcoinj.params.TestNet3Params;
import org.bitcoinj.wallet.DeterministicKeyChain;
import org.bitcoinj.wallet.UnreadableWalletException;
import org.bouncycastle.crypto.InvalidCipherTextException;
import swap.bitcoin.DeterministicKeyChainAtomicSwaps;
import swap.client.ClientSwap;
import swap.mixing.MixingService;
import swap.wallet.SwapsWallet;
import swap.gui.GUISwap;
import swap.gui.controller.MainController;
import swap.gui.controller.pages.HistoryController;
import swap.helper.*;
import swap.listener.StartupListener;
import swap.model.*;
import swap.model.request.GetHistoryRequest;
import swap.model.request.ListSellersDirectRequest;
import swap.model.request.ListSellersRequest;
import swap.model.request.ResumeRequest;
import swap.model.request.SwapRequest;
import swap.model.response.GetHistoryResponse;
import swap.model.response.ListSellersResponse;
import swap.process.ProcessGraceful;

import javax.annotation.Nullable;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.file.Files;
import java.nio.file.StandardCopyOption;
import java.util.*;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import java.util.stream.Collectors;

public class AppSwap implements App {
    // GUI swap wallets are named "<swap uuid>-monitoring-wallet" (plus .keys).
    // Deliberately narrower than "contains", so unrelated wallets such as the
    // swap CLI's "swap-tool-blockchain-monitoring-wallet" are never touched.
    private static final Pattern MONITORING_WALLET_NAME = Pattern.compile(
            "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}-monitoring-wallet(?:\\.keys)?$");

    private ConcurrentHashMap<String, ClientSwap> swapClients = new ConcurrentHashMap<>();

    private final List<Multiaddr> rendezvousPeers = Arrays.asList(
            new Multiaddr(Multiaddr.Protocol.DNS4, "discovery.eigenwallet.org", Multiaddr.Protocol.TCP, 443, Multiaddr.Protocol.WSS, Multiaddr.Protocol.P2P, "12D3KooWGRvf7qVQDrNR5nfYD6rKrbgeTi9x8RrbdxbmsPvxL4mw"),
            new Multiaddr(Multiaddr.Protocol.DNS4, "discovery2.eigenwallet.org", Multiaddr.Protocol.TCP, 443, Multiaddr.Protocol.WSS, Multiaddr.Protocol.P2P, "12D3KooWA6cnqJpVnreBVnoro8midDL9Lpzmg8oJPoAGi7YYaamE"),
            new Multiaddr(Multiaddr.Protocol.DNS4, "dht.stealthswap.ninja", Multiaddr.Protocol.TCP, 443, Multiaddr.Protocol.WSS, Multiaddr.Protocol.P2P, "12D3KooWGjcxdpsEWspGGwkQJ9BRJQjtBQFsLk36zJxrXSBPQWov"),
            new Multiaddr(Multiaddr.Protocol.DNS4, "p2p.myexchange.cash", Multiaddr.Protocol.TCP, 443, Multiaddr.Protocol.WSS, Multiaddr.Protocol.P2P, "12D3KooWQQeUXdMkwJo8zESkKo7xek7NtVgqh1Q4RPnJCTmESX1Z")
    );

    private final List<Multiaddr> rendezvousOnionPeers = Arrays.asList(
            new Multiaddr(Multiaddr.Protocol.ONION3, "3xl2zfur4tpebogsrgn3l7l2illzkhwi3755jplmycmn4q77nxsrl6qd.onion", Multiaddr.Protocol.TCP, 8888, null, Multiaddr.Protocol.P2P, "12D3KooWGRvf7qVQDrNR5nfYD6rKrbgeTi9x8RrbdxbmsPvxL4mw"),
            new Multiaddr(Multiaddr.Protocol.ONION3, "m6rboz5lv4wxldgybgox4pr4s6xci3h2exi5nogxaox762xji2gokuad.onion", Multiaddr.Protocol.TCP, 8891, null, Multiaddr.Protocol.P2P, "12D3KooWGjcxdpsEWspGGwkQJ9BRJQjtBQFsLk36zJxrXSBPQWov"),
            new Multiaddr(Multiaddr.Protocol.ONION3, "av2jauifny7dgpvzhsnhra3cwivf6ofaefxvwhhuh5y7hsolabehhaad.onion", Multiaddr.Protocol.TCP, 8888, null, Multiaddr.Protocol.P2P, "12D3KooWA6cnqJpVnreBVnoro8midDL9Lpzmg8oJPoAGi7YYaamE")
    );

    private final List<Multiaddr> rendezvousPeersWithTor = concat(rendezvousOnionPeers, rendezvousPeers);

    // Known makers quoted DIRECTLY, without rendezvous discovery. Keeps maker
    // listing functional while all rendezvous points are unreachable (they are
    // volunteer infrastructure and have gone down fleet-wide before).
    // Source: eigenwallet.org/liquidity crawler, recently-seen entries, snapshot 2026-10-09.
    private final List<String> knownMakers = Arrays.asList(
            "/dns4/xmr.swapexchange.club/tcp/443/wss/p2p/12D3KooWP8M6ororMZkCTyEKoY2m6MTwX8SHm7NgAcwFqKnFPPbR",
            "/onion3/u7akjfnrz4bjlvole5erjiok6wysthz6xgntxglvx2m7br3663wvyuqd:9939/p2p/12D3KooWRfthPEkt7TcbG1oyrfg2mTQ2oDHxUiipMUwGHdS7JFec",
            "/onion3/geypac53etgwloptq36rjbtccrahji42vrupeqnfdjo3f42gpmxx44yd:9939/p2p/12D3KooWJ1xBqcvVTJQMcvhNvrsLvPS2gT3jCGBW5cX4x2tssWvt",
            "/onion3/xcprxnhtix4zegs3uej43pxbr67wmi43ixe4m5itism52t6fyxdzx4yd:9939/p2p/12D3KooWA4fNnffnwecJikZBBZecKvupA6p1EdMTSvuD5f9G8NPe",
            "/dns4/whaleswap.club/tcp/443/wss/p2p/12D3KooWNTqfRudnVnTrScS5urcoq5gGbjKfJWXTWzymW4FpfPkL",
            "/ip4/45.84.59.237/tcp/9939/p2p/12D3KooWBNAwQtQu7oaRpsKrdQR1yzwGDBU1mYyLCN75HkWgzpcn",
            "/dns4/untraceable.exchange/tcp/443/wss/p2p/12D3KooWAh7Su84tbFCQfQumZkp1zDwwmu1VJDoUXKnca68B8sLu",
            "/onion3/wyj7mqitouzab4jivxfryc7d4qlmkcazl4zcec7c3brpkk4v6gnpckyd:9939/p2p/12D3KooWCMXk5SiDsHDNynpKJVMnWHbGErwLdBU4EpRSJE65NKRq",
            "/onion3/4txzt6qbycb4k4k5n46bibi34bimqwund7d6mpkka3ucutua7ukfrsid:9939/p2p/12D3KooWCtdUp2aUByAQAojPPsrYwo8juhNJ663nbmEacrHedTLM",
            "/dns4/p2p.btc-xmr.com/tcp/9941/p2p/12D3KooWAFX9kzkhks54mSj6zwWkvcFd5KkHMkBRTb7f3uvNv1uL",
            "/onion3/o5fglsvt6a4rqkfpdup65mdlfgqlmbmgcp7kfudrip2rnaoxooojnfqd:9939/p2p/12D3KooWCK2tEB8wUVPpdmTZWkkc6wux5L1cHUebvNSywokCeSSF",
            "/dns4/p2p.myexchange.cash/tcp/443/wss/p2p/12D3KooWQQeUXdMkwJo8zESkKo7xek7NtVgqh1Q4RPnJCTmESX1Z"
    );

    private static <T> List<T> concat(List<T> a, List<T> b) {
        List<T> combined = new ArrayList<>(a);
        combined.addAll(b);
        return Collections.unmodifiableList(combined);
    }

    private static boolean isWindows() {
        return System.getProperty("os.name").toLowerCase().contains("windows");
    }

    /** The forwarder binary in the resources: daemon_pool_proxy.exe on Windows, as asb and price_ticker. */
    static String forwarderResource(boolean windows) {
        return windows ? "/binaries/daemon_pool_proxy.exe" : "/binaries/daemon_pool_proxy";
    }

    private static String forwarderResource() {
        return forwarderResource(isWindows());
    }

    /** Windows only runs a program whose file name ends in .exe. */
    static String forwarderFileName(String base, boolean windows) {
        return windows ? base + ".exe" : base;
    }

    private static String forwarderFileName(String base) {
        return forwarderFileName(base, isWindows());
    }

    public List<String> getKnownMakers() {
        return knownMakers;
    }

    public static int UNINITIALIZED_TOR_PORT = -1;
    private static String swapRootDir = null;

    /*** ELECTRUM SERVERS ***/
    // All curated pools live in node-directory.json (packaged resource,
    // overridable by a file of the same name in the app data dir) — see
    // swap.helper.NodeDirectory. Order = forwarder priority.
    private final List<ElectrumServer> mainnetElectrumServers =
            NodeDirectory.electrum(ElectrumServer.Network.MAINNET, false);

    private final List<ElectrumServer> mainnetElectrumOnionServers =
            NodeDirectory.electrum(ElectrumServer.Network.MAINNET, true);
    // Onion-first: no exit involved in onion dials, so they get priority;
    // SSL clearnet entries are the encrypted fallback layer.
    private final List<ElectrumServer> mainnetElectrumServersWithTor = concat(mainnetElectrumOnionServers, mainnetElectrumServers);

    private final List<ElectrumServer> testnetElectrumServers =
            NodeDirectory.electrum(ElectrumServer.Network.TESTNET, false);
    private final List<ElectrumServer> testnetElectrumOnionServers =
            NodeDirectory.electrum(ElectrumServer.Network.TESTNET, true);
    // onion first, then the clearnet ssl/tcp servers: testnet onions come and go (both were down on
    // 2026-11-12 while blackie and aranguren answered), so leaving clearnet out left nothing working
    private final List<ElectrumServer> testnetElectrumServersWithTor = concat(testnetElectrumOnionServers, testnetElectrumServers);

    /*** MONERO DAEMONS ***/
    private final List<XmrNode> mainnetMoneroDaemons =
            NodeDirectory.monero(XmrNode.Network.MAINNET, false);
    private final List<XmrNode> mainnetMoneroOnionDaemons =
            NodeDirectory.monero(XmrNode.Network.MAINNET, true);
    private final List<XmrNode> mainnetMoneroDaemonsWithTor = concat(mainnetMoneroDaemons, mainnetMoneroOnionDaemons);
    private final List<XmrNode> stagenetMoneroDaemons =
            NodeDirectory.monero(XmrNode.Network.STAGENET, false);
    private final List<XmrNode> stagenetMoneroOnionDaemons =
            NodeDirectory.monero(XmrNode.Network.STAGENET, true);
    private final List<XmrNode> stagenetMoneroDaemonsWithTor = concat(stagenetMoneroDaemons, stagenetMoneroOnionDaemons);

    public File rpcRootDir = null;
    private DeterministicKeyChain keyChain = null;
    private String electrumServer = null;
    private String moneroDaemon = null;
    @Nullable
    private String passphrase = null;
    private StartupListener startupListener = null;
    private WalletSupplierImpl walletSupplier = null;
    private WalletUtxoProvider walletUtxoProvider = null;

    public AppSwap(@org.jetbrains.annotations.Nullable String passphrase, StartupListener startupListener) {
        this.passphrase = passphrase;
        this.startupListener = startupListener;
        setupConfig();
    }

    public static File getSwapRootDir() {
        if (swapRootDir == null) {
            swapRootDir = getDataDir();
            File first = new File(swapRootDir);
            if (!first.exists()) first.mkdirs();
            HelperProperties.restrictDirectoryToOwner(first.toPath());
            HelperProperties.ensureSwapDatabaseFile(first.toPath());
        }
        File dir = new File(swapRootDir);
        if (!dir.exists()) dir.mkdirs();
        return dir;
    }

    private static native String getDataDir();

    private void setupConfig() {
        if (HelperProperties.moneroDaemon == null || HelperProperties.moneroDaemon.isEmpty()) {
            // initial default
            moneroDaemon = HelperDaemonPool.pickDefault(getMoneroDaemonUrls(), new Random());

            // save to properties file
            try {
                HelperProperties.setProperty(HelperProperties.KEY_MONERO_DAEMON, moneroDaemon);
            } catch (IOException e) {
                System.out.println("Failed update atomic-swaps.properties file.");
                throw new RuntimeException(e);
            }
        } else {
            // get saved config
            moneroDaemon = HelperProperties.moneroDaemon;
        }

        if (HelperProperties.electrumServer == null || HelperProperties.electrumServer.isEmpty()) {
            // A first-launch default comes from the curated list only: servers found through the
            // directory are offered afterwards, never chosen for the user.
            List<ElectrumServer> nodes = getCuratedElectrumServers();
            List<ElectrumServer> autoPick = HelperProperties.isUseTor()
                    ? nodes.stream().filter(node -> !node.url().startsWith("ssl://")).collect(Collectors.toList())
                    : nodes;
            if (autoPick.isEmpty()) {
                autoPick = nodes;
            }
            int randomNode = new Random().nextInt(autoPick.size());
            ElectrumServer node = autoPick.get(randomNode);
            // initial default
            electrumServer = node.url();

            // save to properties
            try {
                HelperProperties.setProperty(HelperProperties.KEY_ELECTRUM_SERVER, electrumServer);
            } catch (IOException e) {
                System.out.println("Failed update atomic-swaps.properties file.");
                throw new RuntimeException(e);
            }
        } else {
            // get saved config
            electrumServer = HelperProperties.electrumServer;
        }
    }

    public ListSellersResponse listSellers(ListSellersRequest listSellersRequest) {
        String json = listSellers(listSellersRequest.toJson().toString());
        if (json == null) {
            System.err.println("Seller list request returned no response; showing empty list.");
            return ListSellersResponse.empty();
        }
        return ListSellersResponse.fromJson(json);
    }

    public ListSellersResponse listSellersDirect(ListSellersDirectRequest listSellersDirectRequest) {
        String json = listSellersDirect(listSellersDirectRequest.toJson().toString());
        if (json == null) {
            System.err.println("Direct seller list request returned no response; showing empty list.");
            return ListSellersResponse.empty();
        }
        return ListSellersResponse.fromJson(json);
    }

    public GetHistoryResponse getHistory(GetHistoryRequest getHistoryRequest) {
        String json = getHistory(getHistoryRequest.toJson()
                .put("seedBase64", getSeedAsBase64())
                .toString());
        if (json == null) {
            System.err.println("History request returned no response; showing empty history.");
            return GetHistoryResponse.empty();
        }
        return GetHistoryResponse.fromJson(json);
    }

    public File getRpcRootDir() {
        return rpcRootDir;
    }

    public Optional<String> getProxy() {
        if (HelperProperties.isUseTor() && getProxyPort() != UNINITIALIZED_TOR_PORT && !getProxyAddress().isEmpty()) {
            return Optional.of(getProxyAddress() + ":" + getProxyPort());
        } else {
            return Optional.empty();
        }
    }

    public String getProxyAddress() {
        return HelperProperties.isUseTor() ? "127.0.0.1" : "";
    }

    public String getElectrumServer() {
        return electrumServer;
    }

    public void setElectrumServer(String url) {
        electrumServer = url;

        // save config to properties file
        try {
            HelperProperties.setProperty(HelperProperties.KEY_ELECTRUM_SERVER, electrumServer);
            System.out.println("Updated atomic-swaps.properties file - electrumServer:" + electrumServer);
        } catch (IOException e) {
            System.out.println("Failed update atomic-swaps.properties file and save Electrum Server config.");
            throw new RuntimeException(e);
        }
    }

    /** The packaged curated Electrum list for the current network. */
    public List<ElectrumServer> getCuratedElectrumServers() {
        boolean mainnet = getParams() == MainNetParams.get();
        return HelperProperties.isUseTor() ? (mainnet ? mainnetElectrumServersWithTor : testnetElectrumServersWithTor) : (mainnet ? mainnetElectrumServers : testnetElectrumServers);
    }

    /**
     * Every Electrum server the user can pick and the failover pool can use, in a fixed order: the
     * curated list, then the user's own, then the servers found through Dojo Bay that passed the
     * probe (see {@link HelperElectrumDiscovery}).
     */
    public List<ElectrumServer> getElectrumServers() {
        boolean mainnet = getParams() == MainNetParams.get();
        List<ElectrumServer> custom = new ArrayList<>();
        HelperBtcNodesProperties.getInstance().getCustomNodes().forEach((customNode, blank) -> {
            String id = Sha256Hash.of(customNode.getBytes()).toString().substring(0, 8);
            ElectrumServer.Type type = ElectrumServer.Type.WEB_CORS;
            ElectrumServer.Network network = mainnet ? ElectrumServer.Network.MAINNET : ElectrumServer.Network.TESTNET;
            if (customNode.contains(".onion"))
                type = ElectrumServer.Type.ONION;
            custom.add(new ElectrumServer(id, customNode, type, network));
        });
        return mergeElectrumServers(getCuratedElectrumServers(), custom, HelperElectrumDiscovery.discovered(!mainnet));
    }

    /** Curated first, the user's own next (replacing a curated entry with the same URL), discovered last (never replacing). */
    static List<ElectrumServer> mergeElectrumServers(List<ElectrumServer> curated, List<ElectrumServer> custom, List<ElectrumServer> discovered) {
        java.util.LinkedHashMap<String, ElectrumServer> nodes = new java.util.LinkedHashMap<>();
        curated.forEach(server -> nodes.put(server.url(), server));
        custom.forEach(server -> nodes.put(server.url(), server));
        discovered.forEach(server -> nodes.putIfAbsent(server.url(), server));
        return List.copyOf(nodes.values());
    }

    public List<String> getElectrumServerUrls() {
        return getElectrumServers().stream().map(ElectrumServer::url).collect(Collectors.toList());
    }

    public String getMoneroDaemon() {
        return moneroDaemon;
    }

    public void setMoneroDaemon(String url) {
        moneroDaemon = url;
        // save config to properties file
        try {
            HelperProperties.setProperty(HelperProperties.KEY_MONERO_DAEMON, moneroDaemon);
            System.out.println("Updated atomic-swaps.properties file - moneroDaemon:" + moneroDaemon);
        } catch (IOException e) {
            System.out.println("Failed update atomic-swaps.properties file and save Monero Daemon config.");
            throw new RuntimeException(e);
        }
    }

    /** The packaged curated list for the current network, without the user's own nodes. */
    public List<XmrNode> getCuratedMoneroDaemons() {
        boolean mainnet = getParams() == MainNetParams.get();
        return HelperProperties.isUseTor() ? (mainnet ? mainnetMoneroDaemonsWithTor : stagenetMoneroDaemonsWithTor) : (mainnet ? mainnetMoneroDaemons : stagenetMoneroDaemons);
    }

    /**
     * The Monero witness nodes for a swap started or resumed now, and whether they are required
     * (see {@link HelperMoneroWitnesses}).
     */
    public HelperMoneroWitnesses.Selection selectMoneroWitnesses() {
        return HelperMoneroWitnesses.select(
                !HelperNetwork.isTestnet(getParams()),
                HelperMoneroWitnesses.testNetworkOptIn(),
                getMoneroDaemon(),
                HelperProperties.isMoneroNodeOwn(),
                HelperProperties.isDaemonFailoverInEffect(),
                getCuratedMoneroDaemons(),
                new java.security.SecureRandom());
    }

    public List<XmrNode> getMoneroDaemons() {
        boolean mainnet = getParams() == MainNetParams.get();
        List<XmrNode> defaults = getCuratedMoneroDaemons();
        HashMap<String, XmrNode> nodes = new HashMap<>();

        defaults.forEach(xmrNode -> nodes.put(xmrNode.url(), xmrNode));

        HelperXmrNodesProperties.getInstance().getCustomNodes().forEach((customNode, blank) -> {
            String id = Sha256Hash.of(customNode.getBytes()).toString().substring(0, 8);
            XmrNode.Type type = XmrNode.Type.WEB_CORS;
            XmrNode.Network network = mainnet ? XmrNode.Network.MAINNET : XmrNode.Network.STAGENET;
            if (customNode.contains(".onion"))
                type = XmrNode.Type.ONION;
            else if (customNode.contains(".i2p"))
                type = XmrNode.Type.I2P;
            XmrNode xmrNode = new XmrNode(id, customNode, type, network);
            nodes.put(customNode, xmrNode);
        });

        return nodes.values().stream().toList();
    }

    public List<String> getMoneroDaemonUrls() {
        return getMoneroDaemons().stream().map(XmrNode::url).collect(Collectors.toList());
    }

    public List<Multiaddr> getRendezvousPeers() {
        HashMap<String, Multiaddr> peers = new HashMap<>();
        List<Multiaddr> defaults = HelperProperties.isUseTor() ? rendezvousPeersWithTor : rendezvousPeers;
        defaults.forEach(defaultMultiaddr -> peers.put(defaultMultiaddr.toString(), defaultMultiaddr));
        HelperRendezvousPeersProperties.getInstance().getCustomRendezvousPeers().forEach((s, customMultiaddr) -> peers.put(customMultiaddr.toString(), customMultiaddr));
        String localConfig = System.getenv(LocalRendezvousConfig.ENV);
        LocalRendezvousConfig.fromEnvironment().ifPresentOrElse(
                local -> peers.put(local.toString(), local),
                () -> {
                    if (localConfig != null && !localConfig.isBlank()) {
                        System.err.println("Ignoring invalid " + LocalRendezvousConfig.ENV + ": " + localConfig);
                    }
                });
        return peers.values().stream().toList();
    }

    public int getProxyPort() {
        Tor tor = Tor.getDefault();
        if (tor != null) {
            try {
                return tor.getProxy("127.0.0.1").getPort();
            } catch (TorCtlException e) {
                return UNINITIALIZED_TOR_PORT;
            }
        } else {
            return UNINITIALIZED_TOR_PORT;
        }
    }

    public DeterministicKeyChain getKeyChain() {
        return keyChain;
    }

    public String getSeedAsBase64() {
        DeterministicKeyChainAtomicSwaps keyChain = (DeterministicKeyChainAtomicSwaps) getKeyChain();
        if (keyChain == null || keyChain.getSeed() == null) return "";
        return Base64.getEncoder().encodeToString(keyChain.getSeed().getSeedBytes());
    }

    public NetworkParameters getParams() {
        boolean testnet = "TESTNET".equals(HelperProperties.network);
        return testnet ? TestNet3Params.get() : MainNetParams.get();
    }

    public boolean getRunningSwap() {
        AtomicBoolean anyRunning = new AtomicBoolean(false);
        swapClients.forEach((s, clientSwap) -> {
            if(clientSwap.running.get()) {
                anyRunning.set(true);
            }
        });
        return anyRunning.get();
    }

    public void setRunningSwap(boolean status) {
        swapClients.forEach((s, clientSwap) -> {
            if(!status)
                clientSwap.stop();
        });
    }

    /**
     * Removes monitoring wallets left in an incomplete state by a previous
     * shutdown: monero-wallet-rpc writes the .keys file, then the cache; a
     * cache without .keys can neither be opened (file_not_found) nor
     * re-created (create_wallet fails with file_exists), which permanently
     * blocks swap resume.
     *
     * Only incomplete pairs are touched. A complete pair may belong to a swap
     * that is still running (or about to be resumed), so it is left alone.
     *
     * Must be called after any previous wallet-rpc has been shut down and
     * before the new one starts, otherwise a late wallet save can put the
     * stale state straight back.
     */
    static int deleteStaleMonitoringWalletFiles(File moneroWalletsDir) {
        File[] files = moneroWalletsDir.listFiles();
        if (files == null) return 0;

        int deleted = 0;
        Set<String> inspectedBases = new HashSet<>();
        for (File file : files) {
            String name = file.getName();
            Matcher matcher = MONITORING_WALLET_NAME.matcher(name);
            if (!file.isFile() || !matcher.matches()) continue;

            String base = name.endsWith(".keys")
                    ? name.substring(0, name.length() - ".keys".length())
                    : name;
            if (!inspectedBases.add(base)) continue;

            File cache = new File(moneroWalletsDir, base);
            File keys = new File(moneroWalletsDir, base + ".keys");
            if (cache.isFile() && keys.isFile()) continue;

            if (cache.isFile() && cache.delete()) deleted++;
            if (keys.isFile() && keys.delete()) deleted++;
        }
        return deleted;
    }

    @Override
    public void start() {
        if (startupListener == null) throw new RuntimeException("startupListener is null.");
        try {
            File moneroWalletsDir = new File(getRpcRootDir(), "monero-data/" + HelperProperties.network.toLowerCase());
            if (!moneroWalletsDir.exists()) moneroWalletsDir.mkdirs();
            this.keyChain = HelperWallet.createKeyChain(HelperProperties.mnemonicEncrypted, passphrase, HelperProperties.hasPassphrase);
            HelperProperties.unlockSecrets(passphrase);
            setupWalletSupplier();
        } catch (UnreadableWalletException | InvalidCipherTextException | DecryptionException | IOException e) {
            throw new RuntimeException(e);
        }

        Platform.runLater(startupListener::onClientStarted);
    }

    private void setupWalletSupplier() {
        // Extract mnemonic words from decrypted seed
        List<String> mnemonic = getKeyChain().getSeed().getMnemonicCode();
        String bip39Passphrase = HelperProperties.hasPassphrase ? passphrase : "";
        String dojoUrl = HelperProperties.dojoUrl;
        String dojoApiKey = HelperProperties.dojoApiKey;

        // SwapsWallet wraps extlibj HD_Wallet + WalletSupplierImpl + BackendApi (Dojo)
        SwapsWallet swapsWallet = new SwapsWallet(mnemonic, bip39Passphrase, getParams(), dojoUrl, dojoApiKey);

        // Expose walletSupplier for legacy callers (WithdrawController zpubs, SwapsController refund address)
        walletSupplier = swapsWallet.getWalletSupplier();
        walletUtxoProvider = new WalletUtxoProvider(getParams(), walletSupplier);

        // Periodic balance updates via Dojo
        swapsWallet.startPeriodicBalanceUpdates();

        // Whirlpool mixing (of redeemed swap BTC in the ASB account) is intentionally NOT
        // started here - it's tied to the ASB's own start/stop lifecycle instead (see
        // AsbListenerImpl.onAsbInitialized / AppAsb.stop), since it only ever makes sense
        // to run while the ASB is actually up.
    }

    // TODO: Re-add when Dojo integration is wired through SwapsWallet
//    protected BackendApi computeBackendApi(NetworkParameters params, IBackendClient httpClient) {
//        String dojoApiKey = SwapsPairing.getInstance().getDojoApiKey();
//        String dojoUrl = SwapsPairing.getInstance().getDojoUrl();
//        return BackendApi.newBackendApiDojo(httpClient, dojoUrl, dojoApiKey);
//    }

    /** A log line when more than one swap is unfinished: only one runs at a time, the rest wait in History. */
    static String otherUnfinishedNote(long unfinished) {
        if (unfinished < 2) {
            return null;
        }
        long others = unfinished - 1;
        return others + (others == 1 ? " other swap is" : " other swaps are")
                + " unfinished. Resume " + (others == 1 ? "it" : "them") + " from History once this one finishes.";
    }

    public void maybeAutoResume(HistoryController historyController) {
        GUISwap.executorService.submit(() -> {
            while (!historyController.initialized) {
                try {
                    Thread.sleep(100L);
                } catch (InterruptedException e) {
                    throw new RuntimeException(e);
                }
            }
            Optional<SwapData> firstIncompleteSwap = historyController.historyObservableList.stream().filter(swapData -> {
                // BTC_REDEEMED is NOT terminal for bob: the XMR lock sweep still has to
                // run (the state machine reaches XMR_REDEEMED only after sweep_all).
                // Skipping it here would strand the swept-XMR step for every completed
                // swap. XMR_REDEEMED (both legs finished) and the failure states are the
                // real terminals. Single source of truth: HistoryController's list.
                return !HistoryController.isNonResumableStatus(swapData.status());
            }).findAny();

            long unfinished = historyController.historyObservableList.stream()
                    .filter(swapData -> !HistoryController.isNonResumableStatus(swapData.status())).count();
            MainController.getInstance().updateGui(() -> {
                firstIncompleteSwap.ifPresent(swapData -> {
                    MainController.getInstance().historyPage();
                    historyController.resumeSwap(swapData.swapId());
                    String more = otherUnfinishedNote(unfinished);
                    if (more != null) {
                        MainController.getInstance().printSwapLogLn(LogType.INFO, more, false);
                    }
                });
            });
        });
    }

    @Override
    public void stop() {
        MainController mainController = MainController.getInstance();
        if (mainController != null) {
            mainController.printSwapLogLn(LogType.HIGHLIGHT, "[SWAP_CLIENT] Running shutdown process...", false);
        }
        for (ClientSwap swapClient : swapClients.values()) {
            swapClient.stop();
        }

        MixingService.disable();

        stopDaemonPoolForwarder();
        stopElectrumPoolForwarder();

        // Monitoring wallets are deliberately NOT deleted here: stop() runs in
        // a shutdown hook that can finish long after a new app instance has
        // started (and opened the same wallet), and monero-wallet-rpc can save
        // the cache after the delete. Both races leave a cache without keys.
        // Stale pairs are swept in AppXmrRpc.startMoneroRpc instead, after the
        // previous rpc has exited and before the new one starts.
    }

    @Override
    public void restart() {
        // stub
    }

    // Local daemon-pool forwarder (daemon_pool_proxy): both monero-wallet-rpc
    // processes point at 127.0.0.1:<port>; the forwarder dials the pool
    // members through Tor SOCKS and fails over on dead circuits so the
    // wallet RPC never sits in its multi-minute reconnect backoff.
    private Process daemonPoolProcess = null;
    private int daemonPoolPort = -1;
    private String daemonPoolKey = null;

    /**
     * Starts (or reuses) the local daemon-pool forwarder.
     *
     * Pool composition: the saved daemon is always the highest-priority
     * member. When daemonFailover is enabled (opt-in), the remaining
     * curated daemons are appended as fallbacks. With it off the pool is
     * PINNED to the saved daemon: no switching, curated default or not.
     *
     * The forwarder is reused only while its pool configuration is unchanged;
     * when the saved daemon or failover setting changes (e.g. an XMR node
     * change saved in Settings), the forwarder is rotated onto the new pool on
     * the same local port so wallet-RPC clients keep the same endpoint.
     *
     * @return the forwarder endpoint for monero-wallet-rpc
     *         (http://127.0.0.1:port), or null when Tor is off or the
     *         forwarder could not start (callers fall back to the direct
     *         daemon + Tor proxy behavior).
     */
    public synchronized String startDaemonPoolForwarder() {
        int torPort = getProxyPort();
        if (torPort == UNINITIALIZED_TOR_PORT) {
            return null;
        }

        String savedDaemon = getMoneroDaemon();
        List<String> curated = getMoneroDaemonUrls();
        boolean failoverAllowed = HelperProperties.isDaemonFailoverInEffect();

        List<String> pool = new ArrayList<>();
        pool.add(savedDaemon);
        if (failoverAllowed) {
            // Mainnet: only onion and https daemons are added (see HelperDaemonPool).
            pool.addAll(HelperDaemonPool.failoverCandidates(curated, savedDaemon, !HelperNetwork.isTestnet(getParams())));
        }
        String poolKey = (failoverAllowed ? "failover|" : "pinned|") + String.join(",", pool);

        if (daemonPoolProcess != null && daemonPoolProcess.isAlive()) {
            if (poolKey.equals(daemonPoolKey)) {
                return "http://127.0.0.1:" + daemonPoolPort;
            }
            System.out.println("[DAEMON_POOL] pool configuration changed; rotating forwarder");
            daemonPoolProcess.destroy();
            try {
                daemonPoolProcess.waitFor();
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
            daemonPoolProcess = null;
        }

        try {
            File binary = getResourceAsFile(forwarderResource());
            if (binary == null || !binary.exists()) {
                System.out.println("[DAEMON_POOL] binary missing from resources; falling back to direct daemon connection");
                return null;
            }
            File destination = new File(getRpcRootDir(), forwarderFileName("daemon_pool_proxy"));
            // A forwarder left by an earlier run (crash, kill) keeps running against a Tor that
            // no longer exists; stop it before replacing the binary and starting a new one.
            int strays = swap.process.ProcessGraceful.stopStrays(destination.getCanonicalPath(), "daemon_pool_proxy");
            if (strays > 0) {
                System.out.println("[FORWARDER] stopped " + strays + " stale daemon_pool_proxy process(es) from an earlier run");
            }
            Files.copy(binary.toPath(), destination.toPath(), StandardCopyOption.REPLACE_EXISTING);
            destination.setReadable(true);
            destination.setExecutable(true);
            destination.setWritable(false);
            destination.setWritable(true, true);

            int port;
            if (daemonPoolPort > 0) {
                port = daemonPoolPort;
            } else {
                try (ServerSocket socket = new ServerSocket(0)) {
                    port = socket.getLocalPort();
                }
            }

            ArrayList<String> cmd = new ArrayList<>();
            cmd.add(destination.getCanonicalPath());
            cmd.add("--listen");
            cmd.add("127.0.0.1:" + port);
            cmd.add("--tor-socks");
            cmd.add("127.0.0.1:" + torPort);
            cmd.add("--daemons");
            cmd.add(String.join(",", pool));
            if (!failoverAllowed) {
                cmd.add("--pinned");
            }

            daemonPoolProcess = new ProcessBuilder(cmd).inheritIO().start();
            daemonPoolPort = port;

            boolean listening = false;
            for (int attempt = 0; attempt < 20 && !listening; attempt++) {
                try (Socket probe = new Socket("127.0.0.1", port)) {
                    listening = true;
                } catch (IOException ignored) {
                    Thread.sleep(250);
                }
            }
            if (!listening || !daemonPoolProcess.isAlive()) {
                daemonPoolProcess.destroy();
                daemonPoolProcess = null;
                daemonPoolPort = -1;
                daemonPoolKey = null;
                System.out.println("[DAEMON_POOL] forwarder did not become ready; falling back to direct daemon connection");
                return null;
            }

            daemonPoolKey = poolKey;
            System.out.println("[DAEMON_POOL] active: " + pool.size() + " daemon(s), failover " + (failoverAllowed ? "automatic (onion-first pool)" : "pinned to the configured daemon"));
            return "http://127.0.0.1:" + daemonPoolPort;
        } catch (Exception e) {
            System.out.println("[DAEMON_POOL] could not start forwarder; falling back to direct daemon connection: " + e.getMessage());
            return null;
        }
    }

    public synchronized void stopDaemonPoolForwarder() {
        if (daemonPoolProcess != null && daemonPoolProcess.isAlive()) {
            ProcessGraceful.shutdown(daemonPoolProcess);
        }
        daemonPoolProcess = null;
        daemonPoolPort = -1;
        daemonPoolKey = null;
    }

    // Electrum pool forwarder: same sidecar binary, fronting the electrum
    // servers for the ASB's bitcoin wallet (which connects locally, no SOCKS —
    // the forwarder dials every entry through Tor itself). Consent rules mirror
    // the Monero daemon pool: electrumFailover is authoritative — off pins the
    // selected server (curated or custom), on opens the curated pool.
    private Process electrumPoolProcess = null;
    private int electrumPoolPort = -1;
    private String electrumPoolKey = null;

    public String getSafeElectrumEndpoint(String configured) {
        // Prefer a direct onion tcp:// server over Tor SOCKS (legacy behavior, and
        // what the ASB already does). The local forwarder stays as the fallback for
        // configurations with no direct tcp:// server, e.g. a pinned ssl:// server
        // that must not be dialed directly.
        String direct = selectDirectElectrumEndpoint(configured, getDirectElectrumPoolUrls());
        if (direct != null) {
            return direct;
        }
        String forwarded = startElectrumPoolForwarder();
        if (forwarded != null) {
            return forwarded;
        }
        rejectDirectElectrumSsl(configured);
        return configured;
    }

    /**
     * The tcp:// server the wallet should dial itself: the configured server when
     * it is already tcp://, otherwise the first tcp:// member of the pool, or
     * null when there is none (the caller then falls back to the forwarder).
     */
    static String selectDirectElectrumEndpoint(String configured, List<String> directPool) {
        if (configured != null && configured.startsWith("tcp://")) {
            return configured;
        }
        return directPool == null || directPool.isEmpty() ? null : directPool.get(0);
    }

    static void rejectDirectElectrumSsl(String configured) {
        if (configured != null && configured.startsWith("ssl://")) {
            throw new IllegalStateException("Direct Electrum TLS is disabled; start the local " +
                    "forwarder or use an onion tcp:// server");
        }
    }

    /**
     * Electrum endpoints the ASB can dial itself through its SOCKS proxy.
     * Direct TLS is rejected by the ASB, so only tcp:// endpoints qualify.
     * Passing the whole pool lets the ASB's own balancer fail over between
     * servers instead of pinning one local forwarder port that can die.
     */
    public List<String> getDirectElectrumPoolUrls() {
        if (getProxyPort() == UNINITIALIZED_TOR_PORT) {
            return List.of();
        }
        return electrumPoolUrls(
                getElectrumServer(),
                getElectrumServers().stream().map(ElectrumServer::url).collect(Collectors.toList()),
                HelperProperties.isElectrumFailoverInEffect());
    }

    /**
     * The tcp:// servers a wallet may use. Failover off means exactly the
     * user's saved server (one endpoint, never anything else); failover on adds
     * the curated pool after it.
     */
    static List<String> electrumPoolUrls(String savedServer, List<String> curated, boolean failover) {
        List<String> pool = new ArrayList<>();
        if (savedServer != null && !savedServer.isBlank()) {
            pool.add(savedServer);
        }
        if (failover && curated != null) {
            pool.addAll(curated);
        }
        return pool.stream()
                .filter(url -> url != null && url.startsWith("tcp://"))
                .distinct()
                .collect(Collectors.toList());
    }

    /**
     * Electrum endpoints handed to the taker's swap wallet so it can fail over
     * between servers when one stalls (swaps f7a162f7 and bf5f78fa: a single
     * onion server stalled and the taker had nowhere to go). Only tcp://
     * endpoints qualify; the saved server is first, and the curated pool is
     * added only while electrum failover is enabled.
     */
    public List<String> getTakerElectrumPoolUrls() {
        return getDirectElectrumPoolUrls();
    }

    public synchronized String startElectrumPoolForwarder() {
        int torPort = getProxyPort();
        if (torPort == UNINITIALIZED_TOR_PORT) {
            return null;
        }

        String savedServer = getElectrumServer();
        List<String> curated = getElectrumServers().stream().map(ElectrumServer::url).collect(Collectors.toList());
        boolean failoverAllowed = HelperProperties.isElectrumFailoverInEffect();

        List<String> pool = new ArrayList<>();
        pool.add(savedServer);
        if (failoverAllowed) {
            curated.stream().filter(url -> !url.equals(savedServer)).forEach(pool::add);
        }
        String poolKey = (failoverAllowed ? "failover|" : "pinned|") + String.join(",", pool);

        if (electrumPoolProcess != null && electrumPoolProcess.isAlive()) {
            if (poolKey.equals(electrumPoolKey)) {
                return "tcp://127.0.0.1:" + electrumPoolPort;
            }
            System.out.println("[ELECTRUM_POOL] pool configuration changed; rotating forwarder");
            electrumPoolProcess.destroy();
            try {
                electrumPoolProcess.waitFor();
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
            electrumPoolProcess = null;
        }

        try {
            File binary = getResourceAsFile(forwarderResource());
            if (binary == null || !binary.exists()) {
                System.out.println("[ELECTRUM_POOL] binary missing from resources; ASB uses its electrum server directly");
                return null;
            }
            File destination = new File(getRpcRootDir(), forwarderFileName("daemon_pool_proxy_electrum"));
            // A forwarder left by an earlier run (crash, kill) keeps running against a Tor that
            // no longer exists; stop it before replacing the binary and starting a new one.
            int strays = swap.process.ProcessGraceful.stopStrays(destination.getCanonicalPath(), "daemon_pool_proxy_electrum");
            if (strays > 0) {
                System.out.println("[FORWARDER] stopped " + strays + " stale daemon_pool_proxy_electrum process(es) from an earlier run");
            }
            Files.copy(binary.toPath(), destination.toPath(), StandardCopyOption.REPLACE_EXISTING);
            destination.setReadable(true);
            destination.setExecutable(true);
            destination.setWritable(false);
            destination.setWritable(true, true);

            int port;
            if (electrumPoolPort > 0) {
                port = electrumPoolPort;
            } else {
                try (ServerSocket socket = new ServerSocket(0)) {
                    port = socket.getLocalPort();
                }
            }

            ArrayList<String> cmd = new ArrayList<>();
            cmd.add(destination.getCanonicalPath());
            cmd.add("--listen");
            cmd.add("127.0.0.1:" + port);
            cmd.add("--tor-socks");
            cmd.add("127.0.0.1:" + torPort);
            cmd.add("--daemons");
            cmd.add(String.join(",", pool));
            cmd.add("--kind");
            cmd.add("electrum");
            if (!failoverAllowed) {
                cmd.add("--pinned");
            }

            electrumPoolProcess = new ProcessBuilder(cmd).inheritIO().start();
            electrumPoolPort = port;

            boolean listening = false;
            for (int attempt = 0; attempt < 20 && !listening; attempt++) {
                try (Socket probe = new Socket("127.0.0.1", port)) {
                    listening = true;
                } catch (IOException ignored) {
                    Thread.sleep(250);
                }
            }
            if (!listening || !electrumPoolProcess.isAlive()) {
                electrumPoolProcess.destroy();
                electrumPoolProcess = null;
                electrumPoolPort = -1;
                electrumPoolKey = null;
                System.out.println("[ELECTRUM_POOL] forwarder did not become ready; ASB uses its electrum server directly");
                return null;
            }

            electrumPoolKey = poolKey;
            System.out.println("[ELECTRUM_POOL] active: " + pool.size() + " server(s), failover " + (failoverAllowed ? "automatic" : "pinned to the configured server"));
            return "tcp://127.0.0.1:" + electrumPoolPort;
        } catch (Exception e) {
            System.out.println("[ELECTRUM_POOL] could not start forwarder; ASB uses its electrum server directly: " + e.getMessage());
            return null;
        }
    }

    public synchronized void stopElectrumPoolForwarder() {
        if (electrumPoolProcess != null && electrumPoolProcess.isAlive()) {
            ProcessGraceful.shutdown(electrumPoolProcess);
        }
        electrumPoolProcess = null;
        electrumPoolPort = -1;
        electrumPoolKey = null;
    }

    private File getResourceAsFile(String resourcePath) {
        try {
            InputStream in = getClass().getResourceAsStream(resourcePath);
            if (in == null) {
                return null;
            }

            File tempFile = File.createTempFile(String.valueOf(in.hashCode()), ".tmp");
            tempFile.deleteOnExit();

            try (FileOutputStream out = new FileOutputStream(tempFile)) {
                byte[] buffer = new byte[1024];
                int bytesRead;
                while ((bytesRead = in.read(buffer)) != -1) {
                    out.write(buffer, 0, bytesRead);
                }
            }
            return tempFile;
        } catch (IOException e) {
            throw new RuntimeException(e);
        }
    }

    public void restartXmrRpcProcesses() {
        for (ClientSwap clientSwap : getSwapClients().values()) {
            clientSwap.restartXmrRpcProcess();
        }
    }

    public ConcurrentHashMap<String, ClientSwap> getSwapClients() {
        return swapClients;
    }

    /**
     * Stops and removes the swap client for a swap that reached a terminal outcome
     * (completed, refunded, aborted, or fatally failed). Stopping the exact client
     * prevents its retry loop from re-invoking a finished swap while leaving any
     * unrelated swap clients untouched.
     */
    public void stopSwapClient(String swapId) {
        if (swapId == null || swapId.isEmpty()) return;
        ClientSwap clientSwap = swapClients.remove(swapId);
        if (clientSwap != null) clientSwap.stop();
    }

    public void buyXmr(SwapRequest swapRequest) {
        // Persist durable account metadata before starting the swap client: if the app
        // dies between start and persist, recovery would refuse to resume (fail-closed),
        // so the swap must not begin until the account record is on disk.
        HelperSwapsDb.getInstance().setSwapsAccount(swapRequest.uuid(), swapRequest.swapsAccount());

        startClient(swapRequest.uuid(), new ClientSwap(this, swapRequest));
    }

    // One client swap at a time: they share the deposit wallet (first review #11). `running` is
    // set inside start(), so checking and starting under one lock leaves no window.
    private synchronized void startClient(String swapId, ClientSwap swapClient) {
        HelperSwapGuard.requireNoRunningSwap(
                swapClients.values().stream().map(client -> client.running).toList());
        swapClients.put(swapId, swapClient);
        swapClient.start();
    }

    private native String listSellers(String json);

    private native String listSellersDirect(String json);

    public void resume(ResumeRequest resumeRequest) {
        startClient(resumeRequest.swapId(), new ClientSwap(this, resumeRequest));
    }

    private native String getHistory(String json);

    public String getPassphrase() {
        return passphrase;
    }

    public WalletSupplierImpl getWalletSupplier() {
        return walletSupplier;
    }

    public WalletUtxoProvider getWalletUtxoProvider() {
        return walletUtxoProvider;
    }
}
