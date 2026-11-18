package swap.gui.controller.pages;

import static org.junit.Assert.assertTrue;

import java.nio.file.Files;
import java.nio.file.Path;
import org.junit.Test;

/** SwapsController cannot load in a test (native library), so these read its source. */
public class SwapsAmountsClearedTest {
    private static String source() throws Exception {
        return Files.readString(Path.of("src/main/java/swap/gui/controller/pages/SwapsController.java"));
    }

    private static String body(String source, String signature) {
        int start = source.indexOf(signature);
        assertTrue(signature + " exists", start >= 0);
        return source.substring(start, Math.min(source.length(), start + 700));
    }

    @Test
    public void disconnectAndClearBothEmptyTheAmountBoxes() throws Exception {
        String source = source();
        // Clear goes through clearSwapDisplay -> resetSwapDisplay, and a Disconnect through resetSwapDisplay
        assertTrue(body(source, "public void resetSwapDisplay()").contains("clearAmounts()"));
        assertTrue(body(source, "private void clearSwapDisplay()").contains("resetSwapDisplay()"));
    }

    @Test
    public void clearAmountsEmptiesBothFieldsWithoutTriggeringTheListeners() throws Exception {
        String clear = body(source(), "private void clearAmounts()");
        assertTrue(clear.contains("amountsSyncing = true"));
        assertTrue(clear.contains("payBtcField.clear()"));
        assertTrue(clear.contains("receiveXmrField.clear()"));
    }
}
