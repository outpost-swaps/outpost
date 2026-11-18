package swap.helper;

import java.net.URI;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Locale;
import java.util.Random;
import java.util.Set;
import swap.model.XmrNode;

/**
 * Chooses the Monero witness nodes for a swap.
 *
 * <p>Bob releases his redeem signature once the Monero node he asks says Alice's lock exists, so a
 * node that lies (or is run by the maker) could take his Bitcoin for nothing. Witnesses are other
 * nodes that must independently agree before he releases it (see {@code swap::monero::witness}).
 *
 * <p>When they are needed:
 * <ul>
 *   <li>On mainnet, always, unless the answer can only come from the user's own node. That is the
 *       case when the saved node is local, or the user declared it their own
 *       ({@code moneroNodeIsOwn}), <em>and</em> failover is off: with failover on, the daemon that
 *       answers can be a public one.
 *   <li>On test networks only when the environment variable {@link #TEST_NETWORK_OPT_IN_ENV} is {@code 1}.
 * </ul>
 * The "Trust Daemon" setting is deliberately not used: it only enables bigger sync batches for
 * speed and can be on for a public node.
 *
 * <p>Witnesses come from the curated list only, are onion or https (a Tor exit relay can rewrite
 * plain HTTP), never the saved node or another node of its operator, and no two share an operator.
 * If too few qualify while they are required, the swap is refused: the check is never skipped.
 */
public final class HelperMoneroWitnesses {
    public static final int COUNT = 2;
    /**
     * Extra candidates sent along with the {@link #COUNT} needed. Each candidate is checked before
     * a swap starts (reachable through Tor, certificate accepted) and the first {@link #COUNT} that
     * work are used, so one node that is down or has a bad certificate does not stop a swap.
     */
    public static final int SPARE = 4;
    public static final String TEST_NETWORK_OPT_IN_ENV = "OUTPOST_MONERO_WITNESSES";

    private HelperMoneroWitnesses() {}

    /**
     * {@code urls} are candidates in order of preference and {@code count} of them are used (the
     * engine checks them first). {@code problem} is set only when witnesses are required and could
     * not be found.
     */
    public record Selection(List<String> urls, boolean required, String problem, int count) {
        public Selection(List<String> urls, boolean required, String problem) {
            this(urls, required, problem, 0);
        }
    }

    public static boolean testNetworkOptIn() {
        return "1".equals(System.getenv(TEST_NETWORK_OPT_IN_ENV));
    }

    public static Selection select(
            boolean mainnet,
            boolean testNetworkOptIn,
            String savedDaemon,
            boolean ownNode,
            boolean failoverAllowed,
            List<XmrNode> curated,
            Random random) {
        boolean answersOnlyFromTheUsersNode = (isLoopback(savedDaemon) || ownNode) && !failoverAllowed;
        boolean required = (mainnet || testNetworkOptIn) && !answersOnlyFromTheUsersNode;
        if (!required) {
            return new Selection(List.of(), false, null);
        }

        String savedOperator = operatorOfSaved(savedDaemon, curated);
        List<XmrNode> candidates = new ArrayList<>();
        Set<String> operators = new HashSet<>();
        List<XmrNode> shuffled = new ArrayList<>(curated);
        Collections.shuffle(shuffled, random);
        for (XmrNode node : shuffled) {
            String operator = operatorKey(node.name(), node.url());
            if (node.url().equals(savedDaemon)
                    || operator.equals(savedOperator)
                    || !HelperDaemonPool.isTamperResistant(node.url())
                    || !operators.add(operator)) {
                continue;
            }
            candidates.add(node);
        }
        if (candidates.size() < COUNT) {
            return new Selection(
                    List.of(),
                    true,
                    "Swapping on mainnet needs " + COUNT + " independent onion or https Monero nodes to double-check "
                            + "the seller's lock, and only " + candidates.size() + " are available. Mark your Monero "
                            + "node as your own in Settings, or add more nodes.");
        }
        int shown = Math.min(candidates.size(), COUNT + SPARE);
        return new Selection(candidates.subList(0, shown).stream().map(XmrNode::url).toList(), true, null, COUNT);
    }

    static boolean isLoopback(String url) {
        String host = hostOf(url);
        return host != null && (host.equals("localhost") || host.equals("127.0.0.1") || host.equals("[::1]") || host.equals("::1"));
    }

    private static String operatorOfSaved(String savedDaemon, List<XmrNode> curated) {
        for (XmrNode node : curated) {
            if (node.url().equals(savedDaemon)) {
                return operatorKey(node.name(), node.url());
            }
        }
        return operatorKey(null, savedDaemon);
    }

    /** "Cake Wallet" and "cakewallet" are one operator; so are "monerujo" and "monerujo.onion". */
    static String operatorKey(String name, String url) {
        String base = name != null && !name.isBlank() ? name : hostOf(url);
        if (base == null) {
            return "";
        }
        return base.toLowerCase(Locale.ROOT).replace(".onion", "").replaceAll("[^a-z0-9]", "");
    }

    private static String hostOf(String url) {
        if (url == null || url.isBlank()) {
            return null;
        }
        try {
            String text = url.trim().toLowerCase(Locale.ROOT);
            return URI.create(text.contains("://") ? text : "http://" + text).getHost();
        } catch (IllegalArgumentException e) {
            return null;
        }
    }
}
