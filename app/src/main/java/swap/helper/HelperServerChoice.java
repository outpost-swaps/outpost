package swap.helper;

import java.io.IOException;
import java.net.URI;
import java.util.List;
import java.util.Locale;
import java.util.Random;
import java.util.stream.Collectors;

/**
 * The Electrum server and Monero node chosen while connecting or creating a wallet, free of any
 * window so it can be tested: checking a typed address, the default pick from the curated list, and
 * saving the choice where {@code AppSwap.setupConfig} and the Settings page read it.
 *
 * <p>Saving happens after the pairing record is written and before the app starts, so the first
 * start already uses the chosen servers instead of a random pick (2026-11-11: a new install picked
 * one stagenet node silently and the first Monero sync failed against it).
 */
public final class HelperServerChoice {
    private HelperServerChoice() {}

    /** The chosen servers; {@code own} marks a server the user runs, as "Personal" in Settings does. */
    public record Choice(String electrumUrl, boolean electrumOwn, String moneroUrl, boolean moneroOwn) {}

    // the property keys Settings' NodeTypeSelector uses for the same choice
    static final String ELECTRUM_OWN_LIST = "electrumOwnServers";
    static final String ELECTRUM_LAST_OWN = "electrumLastOwnServer";
    static final String ELECTRUM_LAST_PUBLIC = "electrumLastPublicServer";
    static final String ELECTRUM_IS_OWN = "electrumServerIsOwn";
    static final String MONERO_OWN_LIST = "moneroOwnNodes";
    static final String MONERO_LAST_OWN = "moneroLastOwnNode";
    static final String MONERO_LAST_PUBLIC = "moneroLastPublicNode";
    static final String MONERO_IS_OWN = "moneroNodeIsOwn";

    /**
     * A typed Electrum address as the app stores it: {@code tcp://host:port} or {@code ssl://host:port}.
     * A bare {@code host:port} means tcp (onion services need no TLS).
     *
     * @throws IllegalArgumentException with a message for the user
     */
    public static String normalizeElectrum(String text) {
        String value = text == null ? "" : text.trim();
        if (value.isEmpty()) {
            throw new IllegalArgumentException("Enter the Electrum server, for example tcp://server.onion:50001.");
        }
        String lower = value.toLowerCase(Locale.ROOT);
        if (!lower.startsWith("tcp://") && !lower.startsWith("ssl://")) {
            if (lower.contains("://")) {
                throw new IllegalArgumentException("An Electrum server starts with tcp:// or ssl://.");
            }
            value = "tcp://" + value;
        }
        return checkHostPort(value, "Electrum server");
    }

    /**
     * A typed Monero node address as the app stores it: {@code http://host:port} or {@code https://host:port}.
     * A bare {@code host:port} means http.
     *
     * @throws IllegalArgumentException with a message for the user
     */
    public static String normalizeMonero(String text) {
        String value = text == null ? "" : text.trim();
        if (value.isEmpty()) {
            throw new IllegalArgumentException("Enter the Monero node, for example http://node.onion:18089.");
        }
        String lower = value.toLowerCase(Locale.ROOT);
        if (!lower.startsWith("http://") && !lower.startsWith("https://")) {
            if (lower.contains("://")) {
                throw new IllegalArgumentException("A Monero node starts with http:// or https://.");
            }
            value = "http://" + value;
        }
        return checkHostPort(value, "Monero node");
    }

    private static String checkHostPort(String url, String what) {
        URI uri;
        try {
            uri = URI.create(url);
        } catch (IllegalArgumentException e) {
            throw new IllegalArgumentException("That " + what + " address is not valid.");
        }
        if (uri.getHost() == null || uri.getHost().isBlank()) {
            throw new IllegalArgumentException("That " + what + " address has no host.");
        }
        if (uri.getPort() < 1 || uri.getPort() > 65535) {
            throw new IllegalArgumentException("Add the port, for example " + uri.getHost() + (what.startsWith("Electrum") ? ":50001" : ":18089") + ".");
        }
        String path = uri.getRawPath();
        if (path != null && !path.isEmpty() && !path.equals("/")) {
            throw new IllegalArgumentException("Enter only the " + what + "'s address and port.");
        }
        return uri.getScheme().toLowerCase(Locale.ROOT) + "://" + uri.getHost() + ":" + uri.getPort();
    }

    /**
     * The Electrum server offered first, as {@code AppSwap.setupConfig} picks it: a random curated
     * server, preferring ones without TLS-through-an-exit ({@code ssl://}) because the app is Tor-only.
     */
    public static String defaultElectrum(List<String> curated, Random random) {
        if (curated == null || curated.isEmpty()) {
            return null;
        }
        List<String> plain = curated.stream().filter(url -> !url.startsWith("ssl://")).collect(Collectors.toList());
        List<String> pool = plain.isEmpty() ? curated : plain;
        return pool.get(random.nextInt(pool.size()));
    }

    /** Saves the choice for the app's first start and for Settings. */
    public static void save(Choice choice) throws IOException {
        if (choice == null) {
            return;
        }
        if (choice.electrumUrl() != null && !choice.electrumUrl().isBlank()) {
            String url = choice.electrumUrl();
            HelperProperties.setProperty(HelperProperties.KEY_ELECTRUM_SERVER, url);
            HelperProperties.electrumServer = url;
            HelperProperties.setProperty(ELECTRUM_OWN_LIST, HelperOwnNodes.with(HelperProperties.getProperty(ELECTRUM_OWN_LIST), url, choice.electrumOwn()));
            HelperProperties.setProperty(choice.electrumOwn() ? ELECTRUM_LAST_OWN : ELECTRUM_LAST_PUBLIC, url);
            HelperProperties.setProperty(ELECTRUM_IS_OWN, String.valueOf(choice.electrumOwn()));
            if (choice.electrumOwn()) {
                HelperBtcNodesProperties.getInstance().addCustomNode(url);
            }
        }
        if (choice.moneroUrl() != null && !choice.moneroUrl().isBlank()) {
            String url = choice.moneroUrl();
            HelperProperties.setProperty(HelperProperties.KEY_MONERO_DAEMON, url);
            HelperProperties.moneroDaemon = url;
            HelperProperties.setProperty(MONERO_OWN_LIST, HelperOwnNodes.with(HelperProperties.getProperty(MONERO_OWN_LIST), url, choice.moneroOwn()));
            HelperProperties.setProperty(choice.moneroOwn() ? MONERO_LAST_OWN : MONERO_LAST_PUBLIC, url);
            HelperProperties.setProperty(MONERO_IS_OWN, String.valueOf(choice.moneroOwn()));
            if (choice.moneroOwn()) {
                HelperXmrNodesProperties.getInstance().addCustomNode(url);
            }
        }
    }
}
