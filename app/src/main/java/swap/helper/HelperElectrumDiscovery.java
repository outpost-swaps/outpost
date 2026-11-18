package swap.helper;

import java.io.File;
import java.net.URI;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import swap.model.ElectrumServer;

/**
 * Electrum servers found through Dojo Bay's directory, offered next to the curated ones.
 *
 * <p>Each verified Dojo may list its own Electrum server ({@code indexer_url}). That address is not
 * covered by the directory's signatures, so nothing found this way is used until it has answered a
 * probe through Tor on the right chain (see {@link HelperElectrumProbe}) and agrees with the others
 * about the chain tip. Only onion servers qualify. The result joins the Settings list and, when
 * Electrum Failover is on, the failover pool; it is never chosen as a first-launch default.
 */
public final class HelperElectrumDiscovery {
    private static final int MAX_CANDIDATES = 12;
    private static final int PARALLEL_PROBES = 4;
    private static final int PROBE_TIMEOUT_MILLIS = 45_000;
    private static final int TIP_TOLERANCE = 2;

    private static volatile List<ElectrumServer> discovered = List.of();
    private static volatile boolean discoveredTestnet;

    private HelperElectrumDiscovery() {}

    /** A server found in the directory: its address and who lists it. */
    public record Candidate(String url, String name) {}

    /** Servers that passed the probe for this network (empty until discovery has finished). */
    public static List<ElectrumServer> discovered(boolean testnet) {
        return discoveredTestnet == testnet ? discovered : List.of();
    }

    public static boolean isDiscovered(String url) {
        return discovered.stream().anyMatch(server -> server.url().equals(url));
    }

    /** Onion {@code tcp://} or {@code ssl://} indexers of verified Dojos on this network, one per host, in a fixed order. */
    public static List<Candidate> candidates(HelperDojoDirectory.Directory directory, boolean testnet) {
        List<Candidate> out = new ArrayList<>();
        Set<String> hosts = new HashSet<>();
        for (HelperDojoDirectory.Entry entry : directory.forNetwork(testnet)) {
            String url = entry.indexerUrl();
            if (url == null) {
                continue;
            }
            String host;
            String scheme;
            try {
                URI uri = URI.create(url.trim());
                host = uri.getHost();
                scheme = uri.getScheme() == null ? "" : uri.getScheme().toLowerCase(Locale.ROOT);
                if (host == null || uri.getPort() < 0) {
                    continue;
                }
            } catch (IllegalArgumentException e) {
                continue;
            }
            boolean usable = (scheme.equals("tcp") || scheme.equals("ssl")) && host.toLowerCase(Locale.ROOT).endsWith(".onion");
            if (usable && hosts.add(host.toLowerCase(Locale.ROOT))) {
                out.add(new Candidate(url.trim(), entry.name()));
            }
        }
        out.sort(Comparator.comparing(Candidate::name, String.CASE_INSENSITIVE_ORDER));
        return out.size() > MAX_CANDIDATES ? out.subList(0, MAX_CANDIDATES) : out;
    }

    /**
     * The probes that answered on the right chain AND report a tip near the others. The reference is
     * the median tip, so one server claiming a far higher tip cannot make the honest ones look behind.
     * With fewer than three answers there is nothing to compare, and all of them count.
     */
    static List<HelperElectrumProbe.Result> healthy(List<HelperElectrumProbe.Result> results) {
        List<HelperElectrumProbe.Result> ok = results.stream().filter(HelperElectrumProbe.Result::ok).toList();
        if (ok.size() < 3) {
            return ok;
        }
        List<Integer> heights = ok.stream().map(HelperElectrumProbe.Result::height).sorted().toList();
        int median = heights.get(heights.size() / 2);
        return ok.stream().filter(result -> Math.abs(result.height() - median) <= TIP_TOLERANCE).toList();
    }

    /** Probes every candidate (a few at a time) and returns the healthy ones as servers. Blocking. */
    static List<ElectrumServer> probe(List<Candidate> candidates, boolean testnet, HelperElectrumProbe.Connector connector, int timeoutMillis) {
        Map<String, HelperElectrumProbe.Result> results = new ConcurrentHashMap<>();
        ExecutorService pool = Executors.newFixedThreadPool(PARALLEL_PROBES, runnable -> {
            Thread thread = new Thread(runnable, "electrum-probe");
            thread.setDaemon(true);
            return thread;
        });
        try {
            for (Candidate candidate : candidates) {
                pool.submit(() -> results.put(candidate.url(), HelperElectrumProbe.probe(candidate.url(), testnet, connector, timeoutMillis)));
            }
            pool.shutdown();
            pool.awaitTermination((long) timeoutMillis * 3 * (candidates.size() / PARALLEL_PROBES + 1), TimeUnit.MILLISECONDS);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        } finally {
            pool.shutdownNow();
        }
        List<HelperElectrumProbe.Result> good = healthy(new ArrayList<>(results.values()));
        List<ElectrumServer> servers = new ArrayList<>();
        for (Candidate candidate : candidates) { // keep the directory's order
            for (HelperElectrumProbe.Result result : good) {
                if (result.url().equals(candidate.url())) {
                    servers.add(new ElectrumServer("Dojo Bay: " + candidate.name(), candidate.url(), ElectrumServer.Type.ONION,
                            testnet ? ElectrumServer.Network.TESTNET : ElectrumServer.Network.MAINNET));
                }
            }
        }
        return servers;
    }

    /**
     * Runs discovery once on a daemon thread: waits for Tor, loads the directory (the fetch the Dojo
     * list also uses), probes the servers it lists and publishes the healthy ones. Never throws.
     *
     * @param onChange called on that thread when the set of discovered servers changed
     */
    public static void startInBackground(boolean testnet, File cacheFile, Runnable onChange) {
        Thread thread = new Thread(() -> {
            try {
                Integer port = HelperTorPort.waitForSocksPort(120_000);
                if (port == null) {
                    return;
                }
                HelperDojoDirectory.Directory directory = HelperDojoDirectoryLoader.loadLive(port, cacheFile).directory();
                List<ElectrumServer> found = probe(candidates(directory, testnet), testnet, HelperElectrumProbe.throughTor(port), PROBE_TIMEOUT_MILLIS);
                boolean changed = !found.equals(discovered) || discoveredTestnet != testnet;
                discoveredTestnet = testnet;
                discovered = List.copyOf(found);
                System.out.println("[ELECTRUM_DISCOVERY] " + found.size() + " Dojo Bay servers passed the probe");
                if (changed && onChange != null) {
                    onChange.run();
                }
            } catch (Exception e) {
                System.out.println("[ELECTRUM_DISCOVERY] not available: " + HelperDojoDirectoryLoader.reason(e));
            }
        }, "electrum-discovery");
        thread.setDaemon(true);
        thread.start();
    }
}
