package swap.helper;

import java.net.URI;
import java.util.List;

/**
 * Which Monero daemons the failover pool may add after the user's own. All daemon traffic goes
 * through Tor, and a Tor exit relay can rewrite plain HTTP, including the answer that tells Bob
 * Alice's Monero lock exists. On mainnet only daemons that resist that are added: onion
 * services and https.
 */
public final class HelperDaemonPool {

    private HelperDaemonPool() {}

    public static boolean isTamperResistant(String url) {
        if (url == null || url.isBlank()) {
            return false;
        }
        String lower = url.trim().toLowerCase();
        if (lower.startsWith("https://") || lower.startsWith("ssl://")) {
            return true;
        }
        try {
            String host = URI.create(lower.contains("://") ? lower : "http://" + lower).getHost();
            return host != null && host.endsWith(".onion");
        } catch (IllegalArgumentException e) {
            return false;
        }
    }

    /**
     * The daemon a fresh install starts with. A plain-http clearnet node is a poor default behind Tor:
     * an exit relay can tamper with it, and several of them are simply unreachable that way (a first
     * launch that drew one never got its wallet synced). So: an onion node if there is one, else an
     * https node, else whatever is left. Random within the best group, so installs spread out.
     */
    public static String pickDefault(List<String> urls, java.util.Random random) {
        if (urls == null || urls.isEmpty()) {
            return null;
        }
        List<String> onion = urls.stream().filter(HelperDaemonPool::isOnion).toList();
        List<String> resistant = urls.stream().filter(HelperDaemonPool::isTamperResistant).toList();
        List<String> pool = !onion.isEmpty() ? onion : !resistant.isEmpty() ? resistant : urls;
        return pool.get(random.nextInt(pool.size()));
    }

    private static boolean isOnion(String url) {
        if (url == null) {
            return false;
        }
        try {
            String lower = url.trim().toLowerCase();
            String host = URI.create(lower.contains("://") ? lower : "http://" + lower).getHost();
            return host != null && host.endsWith(".onion");
        } catch (IllegalArgumentException e) {
            return false;
        }
    }

    public static List<String> failoverCandidates(List<String> curated, String saved, boolean mainnet) {
        return curated.stream()
                .filter(url -> !url.equals(saved))
                .filter(url -> !mainnet || isTamperResistant(url))
                .toList();
    }
}
