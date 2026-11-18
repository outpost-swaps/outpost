package swap.gui.controller.pages;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.Map;
import org.junit.Test;
import swap.model.Seller;

public class SellerListMergeTest {

    private static Seller seller(String address, double max) {
        return new Seller(address, 0.001, max, 0.0075, "100%");
    }

    private static Map<String, Seller> sellers(String... addresses) {
        Map<String, Seller> map = new LinkedHashMap<>();
        for (String address : addresses) {
            map.put(address, seller(address, 1.0));
        }
        return map;
    }

    @Test
    public void anEmptyTableJustTakesWhatWasFound() {
        Map<String, Seller> result = SellerListMerge.afterRefresh(sellers(), sellers("a", "b"), new HashMap<>());
        assertEquals(2, result.size());
    }

    @Test
    public void aSellerThatMissesOneRefreshStaysAndOneThatMissesThreeLeaves() {
        Map<String, Integer> misses = new HashMap<>();
        Map<String, Seller> table = sellers("a", "b", "c", "d");
        // "d" never answers while the other three do (a good refresh each time)
        for (int round = 1; round <= SellerListMerge.MAX_MISSES; round++) {
            table = SellerListMerge.afterRefresh(table, sellers("a", "b", "c"), misses);
            assertTrue("still listed after " + round + " miss(es)", table.containsKey("d"));
        }
        table = SellerListMerge.afterRefresh(table, sellers("a", "b", "c"), misses);
        assertFalse("gone after one miss too many", table.containsKey("d"));
        assertEquals(3, table.size());
    }

    @Test
    public void aPoorRefreshRemovesNobodyAndDoesNotCountAsAMiss() {
        Map<String, Integer> misses = new HashMap<>();
        Map<String, Seller> table = sellers("a", "b", "c", "d", "e", "f");
        // slow Tor: only one of six answered
        for (int round = 0; round < 5; round++) {
            table = SellerListMerge.afterRefresh(table, sellers("a"), misses);
        }
        assertEquals(6, table.size());
        assertTrue(misses.isEmpty());
    }

    @Test
    public void aSellerThatAnswersAgainStartsFromZeroMisses() {
        Map<String, Integer> misses = new HashMap<>();
        Map<String, Seller> table = sellers("a", "b", "c");
        table = SellerListMerge.afterRefresh(table, sellers("a", "b"), misses);      // "c" misses once
        assertEquals(Integer.valueOf(1), misses.get("c"));
        table = SellerListMerge.afterRefresh(table, sellers("a", "b", "c"), misses); // answers again
        assertFalse(misses.containsKey("c"));
        for (int i = 0; i < SellerListMerge.MAX_MISSES; i++) {
            table = SellerListMerge.afterRefresh(table, sellers("a", "b"), misses);
        }
        assertTrue("two fresh misses are still within the allowance", table.containsKey("c"));
    }

    @Test
    public void theFreshQuoteReplacesTheOldOne() {
        Map<String, Seller> previous = new LinkedHashMap<>();
        previous.put("a", seller("a", 1.0));
        Map<String, Seller> found = new LinkedHashMap<>();
        found.put("a", seller("a", 2.5));
        Map<String, Seller> result = SellerListMerge.afterRefresh(previous, found, new HashMap<>());
        assertEquals(2.5, result.get("a").maxQuantity(), 0.0);
    }
}
