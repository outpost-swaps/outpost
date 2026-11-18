package swap.helper;

import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.Locale;
import org.json.JSONArray;
import org.json.JSONObject;

/**
 * Public Dojos to choose from when connecting a wallet, so a user without their own Dojo does not
 * have to hunt for a URL and API key. The directory is Dojo Bay's (the one Ashigaru Desktop reads):
 * each entry carries a signed message holding the Dojo's pairing, and an entry is only listed if
 * that signature checks out (see {@link HelperDojoSignedMessage}). The URL and key come from the
 * signed text. Where the list comes from (live, saved copy, or the one that shipped) is decided in
 * {@link HelperDojoDirectoryLoader}.
 */
public final class HelperDojoDirectory {
    private static final String RESOURCE = "/dojo-directory.json";

    private HelperDojoDirectory() {}

    /**
     * {@code indexerUrl} is the Dojo's Electrum server, when it has one. It is NOT part of the signed
     * message (the signature covers the pairing only), so it is the directory's word, and a server
     * found this way must pass {@link HelperElectrumProbe} before it is offered.
     */
    public record Entry(String id, String name, String operator, String country, boolean testnet, String url, String apiKey, String indexerUrl) {
        public Entry(String id, String name, String operator, String country, boolean testnet, String url, String apiKey) {
            this(id, name, operator, country, testnet, url, apiKey, null);
        }

        /** For the drop-down: the name, the operator's paynym and the country when known. */
        public String label() {
            StringBuilder label = new StringBuilder(name);
            if (operator != null && !operator.isBlank()) {
                label.append(" (").append(operator).append(")");
            }
            if (country != null && !country.isBlank()) {
                label.append(" \u00B7 ").append(country);
            }
            return label.toString();
        }
    }

    /** {@code skipped} counts active entries dropped because their signature did not check out. */
    public record Directory(String generatedAt, List<Entry> entries, int skipped) {
        public Directory(String generatedAt, List<Entry> entries) {
            this(generatedAt, entries, 0);
        }

        public List<Entry> forNetwork(boolean testnet) {
            return entries.stream()
                    .filter(entry -> entry.testnet() == testnet)
                    .sorted(Comparator.comparing(entry -> entry.name().toLowerCase(Locale.ROOT)))
                    .toList();
        }

        /** The date the directory was generated, e.g. {@code 2026-10-15}. */
        public String snapshotDate() {
            return generatedAt == null || generatedAt.length() < 10 ? "unknown date" : generatedAt.substring(0, 10);
        }
    }

    /** Active, verified entries. Never throws: anything unreadable gives an empty list. */
    public static Directory parse(String json) {
        return parse(json, true);
    }

    /**
     * @param requireSignature true in the app. False only lets tests exercise the listing rules
     *                         (active, network, usable URL and key) without real signatures.
     */
    static Directory parse(String json, boolean requireSignature) {
        List<Entry> entries = new ArrayList<>();
        int skipped = 0;
        String generatedAt = null;
        try {
            JSONObject root = new JSONObject(json);
            generatedAt = root.optString("generated_at", null);
            JSONArray nodes = root.optJSONArray("nodes");
            for (int i = 0; nodes != null && i < nodes.length(); i++) {
                JSONObject node = nodes.getJSONObject(i);
                if (!"active".equals(node.optString("status", ""))) {
                    continue;
                }
                String url;
                String key;
                if (requireSignature) {
                    var signed = HelperDojoSignedMessage.verify(node.optString("signed", null), node.optString("paymentCode", null));
                    if (signed.isEmpty()) {
                        skipped++;
                        continue;
                    }
                    url = signed.get().url();
                    key = signed.get().apiKey();
                } else {
                    JSONObject pairing = node.optJSONObject("payload") != null && node.getJSONObject("payload").optJSONObject("pairing") != null
                            ? node.getJSONObject("payload").getJSONObject("pairing") : node;
                    url = pairing.optString("url", "").trim();
                    key = pairing.optString("apikey", "").trim();
                }
                String lower = url.toLowerCase(Locale.ROOT);
                if (url.isEmpty() || key.isEmpty() || !(lower.startsWith("http://") || lower.startsWith("https://"))) {
                    continue;
                }
                entries.add(new Entry(
                        node.optString("id", url),
                        node.optString("name", "Dojo"),
                        node.optString("paynym", ""),
                        node.optString("country", ""),
                        "testnet".equals(node.optString("network", "mainnet")),
                        url,
                        key,
                        indexer(node)));
            }
        } catch (Exception e) {
            entries.clear();
        }
        return new Directory(generatedAt, List.copyOf(entries), skipped);
    }

    private static String indexer(JSONObject node) {
        String url = node.optString("indexer_url", "").trim();
        if (url.isEmpty()) {
            url = node.optString("detected_indexer", "").trim();
        }
        return url.isEmpty() ? null : url;
    }

    /** The copy that shipped with the app (stale by definition; the offline fallback). Empty if unreadable. */
    public static Directory bundled() {
        try (InputStream in = HelperDojoDirectory.class.getResourceAsStream(RESOURCE)) {
            if (in == null) {
                return new Directory(null, List.of());
            }
            return parse(new String(in.readAllBytes(), StandardCharsets.UTF_8));
        } catch (Exception e) {
            return new Directory(null, List.of());
        }
    }

    static String bundledJson() throws java.io.IOException {
        try (InputStream in = HelperDojoDirectory.class.getResourceAsStream(RESOURCE)) {
            return in == null ? "" : new String(in.readAllBytes(), StandardCharsets.UTF_8);
        }
    }
}
