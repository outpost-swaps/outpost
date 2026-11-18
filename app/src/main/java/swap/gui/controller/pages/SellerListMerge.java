package swap.gui.controller.pages;

import java.util.LinkedHashMap;
import java.util.Map;
import swap.model.Seller;

/**
 * What the Sellers table shows after a refresh. A refresh only finds the sellers that answered in time, and
 * over a slow Tor that can be half of them, so replacing the list with it made the list (and the "Max" total
 * under it) shrink and then recover. A seller that does not answer stays for a few refreshes, and a refresh
 * that found less than half of what was known is treated as a poor one and removes nobody.
 */
final class SellerListMerge {
    /** A seller may miss this many refreshes in a row before it leaves the list. */
    static final int MAX_MISSES = 2;

    private SellerListMerge() {}

    /**
     * @param previous what the table showed when the refresh started
     * @param found    the sellers that answered this time (their quotes win over the old ones)
     * @param misses   refreshes in a row each unanswered seller has missed; updated here
     */
    static Map<String, Seller> afterRefresh(Map<String, Seller> previous, Map<String, Seller> found, Map<String, Integer> misses) {
        Map<String, Seller> result = new LinkedHashMap<>(found);
        boolean poorRefresh = found.size() * 2 < previous.size();
        for (Map.Entry<String, Seller> known : previous.entrySet()) {
            String key = known.getKey();
            if (found.containsKey(key)) {
                continue;
            }
            if (poorRefresh) {
                result.putIfAbsent(key, known.getValue());
                continue;
            }
            int missed = misses.merge(key, 1, Integer::sum);
            if (missed <= MAX_MISSES) {
                result.putIfAbsent(key, known.getValue());
            } else {
                misses.remove(key);
            }
        }
        found.keySet().forEach(misses::remove);
        return result;
    }
}
