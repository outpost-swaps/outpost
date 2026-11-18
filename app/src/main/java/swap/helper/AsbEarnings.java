package swap.helper;

import org.json.JSONObject;

import java.math.BigDecimal;
import java.util.LinkedHashMap;
import java.util.Map;

/**
 * All-time totals for the ASB's swaps, kept across restarts: the gross BTC the
 * redeems paid it and the XMR it sold for them.
 *
 * "Gross" means the value of the Bitcoin redeem output, before the cost of the
 * Monero the maker sent. Each swap is counted once by its swap id, so a resumed
 * swap that reports its earnings again (or a backfill from the log) cannot
 * double count. A swap is stored as {"sats": N, "xmr": piconero}; the first
 * version stored a bare number of satoshis, which is still read.
 */
public final class AsbEarnings {
    static final String DB_NAME = "asb_earnings";
    private static final String SWAPS = "swaps";
    private static final String BACKFILLED = "backfilled";
    private static final String XMR_BACKFILLED = "xmr_backfilled";
    private static final BigDecimal PICO = new BigDecimal("1000000000000");

    /** One swap's figures: satoshis received and piconero of XMR sold. */
    public record Entry(long sats, long xmrPico) {
    }

    private final JSONObject root;

    AsbEarnings(JSONObject root) {
        this.root = root;
    }

    public static AsbEarnings load() {
        return new AsbEarnings(HelperRawJsonDb.getInstance().getDb(DB_NAME));
    }

    @Override
    public String toString() {
        return root.toString();
    }

    public void save() {
        HelperRawJsonDb.getInstance().updateDb(DB_NAME, root);
    }

    /** Records a swap once. Returns true if anything was added or filled in. */
    public boolean record(String swapId, long satoshis, long xmrPico) {
        if (swapId == null || swapId.isBlank() || satoshis <= 0) {
            return false;
        }
        JSONObject swaps = root.optJSONObject(SWAPS);
        if (swaps == null) {
            swaps = new JSONObject();
            root.put(SWAPS, swaps);
        }
        Entry existing = read(swaps, swapId);
        if (existing == null) {
            swaps.put(swapId, new JSONObject().put("sats", satoshis).put("xmr", Math.max(xmrPico, 0)));
            return true;
        }
        // Already counted; only fill in a missing XMR amount (older saves had none).
        if (existing.xmrPico() == 0 && xmrPico > 0) {
            swaps.put(swapId, new JSONObject().put("sats", existing.sats()).put("xmr", xmrPico));
            return true;
        }
        return false;
    }

    public boolean record(String swapId, long satoshis) {
        return record(swapId, satoshis, 0);
    }

    private static Entry read(JSONObject swaps, String swapId) {
        if (!swaps.has(swapId)) {
            return null;
        }
        Object value = swaps.get(swapId);
        if (value instanceof JSONObject object) {
            return new Entry(object.optLong("sats", 0), object.optLong("xmr", 0));
        }
        return new Entry(swaps.optLong(swapId, 0), 0);
    }

    public long totalSatoshis() {
        JSONObject swaps = root.optJSONObject(SWAPS);
        if (swaps == null) {
            return 0;
        }
        long total = 0;
        for (String key : swaps.keySet()) {
            total += read(swaps, key).sats();
        }
        return total;
    }

    public long totalXmrPico() {
        JSONObject swaps = root.optJSONObject(SWAPS);
        if (swaps == null) {
            return 0;
        }
        long total = 0;
        for (String key : swaps.keySet()) {
            total += read(swaps, key).xmrPico();
        }
        return total;
    }

    /**
     * True until the log has been read once for both amounts. A file written
     * before XMR was tracked has `backfilled` but no `xmr_backfilled`, so it is
     * read once more to fill in the XMR of swaps it already counted.
     */
    public boolean needsBackfill() {
        return !root.optBoolean(BACKFILLED, false) || !root.optBoolean(XMR_BACKFILLED, false);
    }

    public void markBackfilled() {
        root.put(BACKFILLED, true);
        root.put(XMR_BACKFILLED, true);
    }

    /** "1.234568" style text for a piconero amount: up to 6 decimals, no trailing zeros. */
    public static String formatXmr(long piconero) {
        BigDecimal xmr = new BigDecimal(piconero).divide(PICO).setScale(6, java.math.RoundingMode.DOWN);
        String text = xmr.stripTrailingZeros().toPlainString();
        return text.contains(".") ? text : text + ".0";
    }

    /** Parses "0.123144019632 XMR" (or just the number) into piconero; 0 if unreadable. */
    static long parseXmrToPico(String text) {
        if (text == null) {
            return 0;
        }
        try {
            String number = text.trim().split("\\s+")[0];
            return new BigDecimal(number).multiply(PICO).longValueExact();
        } catch (Exception e) {
            return 0;
        }
    }

    /**
     * Earnings recorded in the ASB log: ASB_EARNED_BTC lines with a swap id and an
     * integer satoshi amount. The XMR sold comes from the line itself when it
     * has one, otherwise from the swap's liquidity reservation line (the amount
     * reserved is the amount sold). Anything else is ignored.
     */
    public static Map<String, Entry> parseEarnedLines(Iterable<String> lines) {
        Map<String, Long> earned = new LinkedHashMap<>();
        Map<String, Long> earnedXmr = new LinkedHashMap<>();
        Map<String, Long> reservedXmr = new LinkedHashMap<>();
        for (String line : lines) {
            if (line == null || !line.contains("ASB_")) {
                continue;
            }
            try {
                JSONObject fields = new JSONObject(line).optJSONObject("fields");
                if (fields == null) {
                    continue;
                }
                String message = fields.optString("message");
                String swapId = fields.optString("swap_id", "");
                if (swapId.isBlank()) {
                    continue;
                }
                if (message.equals("ASB_EARNED_BTC")) {
                    long sats = fields.optLong("earned", 0);
                    if (sats > 0) {
                        earned.put(swapId, sats);
                        long xmr = fields.optLong("xmr_sold_piconero", 0);
                        if (xmr > 0) {
                            earnedXmr.put(swapId, xmr);
                        }
                    }
                } else if (message.startsWith("ASB_LIQUIDITY_RESERVED") || message.startsWith("ASB_LIQUIDITY_RERESERVED")) {
                    long xmr = parseXmrToPico(fields.optString("amount", fields.optString("xmr", "")));
                    if (xmr > 0) {
                        reservedXmr.putIfAbsent(swapId, xmr);
                    }
                }
            } catch (Exception ignored) {
                // not a JSON log line
            }
        }
        Map<String, Entry> found = new LinkedHashMap<>();
        for (Map.Entry<String, Long> entry : earned.entrySet()) {
            String swapId = entry.getKey();
            long xmr = earnedXmr.getOrDefault(swapId, reservedXmr.getOrDefault(swapId, 0L));
            found.put(swapId, new Entry(entry.getValue(), xmr));
        }
        return found;
    }
}
