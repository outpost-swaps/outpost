package swap.gui.controller.pages;

import static org.junit.Assert.assertEquals;

import java.util.AbstractMap.SimpleEntry;
import java.util.List;
import java.util.Map;
import org.junit.Test;

public class LiquidityInProgressTest {
    private static Map.Entry<String, String> swap(String id, String state) {
        return new SimpleEntry<>(id, state);
    }

    @Test
    public void noSwapsSaysSo() {
        assertEquals(List.of("No swaps in progress"), LiquidityController.inProgressLines(List.of(), 3));
        assertEquals("Swaps in progress: 0", LiquidityController.swapsInProgressLabel(0, false));
    }

    @Test
    public void theNewestSwapsComeFirstAndTheRestAreCounted() {
        List<Map.Entry<String, String>> swaps = List.of(
                swap("11111111-aaaa", "BTC locked"), swap("22222222-bbbb", "XMR locked"),
                swap("33333333-cccc", "Encrypted signature received"), swap("44444444-dddd", "XMR lock in mempool"));
        List<String> lines = LiquidityController.inProgressLines(swaps, 3);
        assertEquals(4, lines.size());
        assertEquals("44444444…  XMR lock in mempool", lines.get(0));
        assertEquals("22222222…  XMR locked", lines.get(2));
        assertEquals("+1 more", lines.get(3));
    }

    @Test
    public void historyCountShowsWhileTheLiveFeedCatchesUp() {
        assertEquals("Swaps in progress: 2 (syncing)", LiquidityController.swapsInProgressLabel(2, true));
    }

    @Test
    public void theCardHasOneLineAndCountsTheRest() {
        assertEquals("No swaps in progress", LiquidityController.inProgressOneLine(List.of())[0]);
        assertEquals("", LiquidityController.inProgressOneLine(List.of())[1]);
        List<Map.Entry<String, String>> swaps = List.of(
                swap("11111111-aaaa", "BTC locked"), swap("22222222-bbbb", "XMR confirming"), swap("33333333-cccc", "Setup"));
        String[] parts = LiquidityController.inProgressOneLine(swaps);
        assertEquals("33333333\u2026  Setup", parts[0]);
        assertEquals("+2 more", parts[1]);
    }
}
