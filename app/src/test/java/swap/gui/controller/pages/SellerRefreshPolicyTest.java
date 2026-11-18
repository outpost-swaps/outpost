package swap.gui.controller.pages;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public class SellerRefreshPolicyTest {

    // Swap f7a162f7: the scheduled refresh fired 137 Tor dials every 2 minutes
    // while the funded swap waited on Electrum through the same Tor.
    @Test
    public void scheduledRefreshIsSkippedWhileASwapIsRunning() {
        assertFalse(SellerRefreshPolicy.shouldRunScheduledRefresh(true));
    }

    @Test
    public void scheduledRefreshRunsWhenIdle() {
        assertTrue(SellerRefreshPolicy.shouldRunScheduledRefresh(false));
    }

    /** Every 2 minutes hammered remote sellers into rate-limiting us (HTTP 429, §458). */
    @Test
    public void scheduledRefreshIsEveryTenMinutes() throws Exception {
        org.junit.Assert.assertEquals(10, SellerRefreshPolicy.SCHEDULED_REFRESH_MINUTES);
        String source = new String(java.nio.file.Files.readAllBytes(java.nio.file.Paths.get(
                "src/main/java/swap/gui/controller/pages/SwapsController.java")));
        assertTrue(source.contains("SellerRefreshPolicy.SCHEDULED_REFRESH_MINUTES, TimeUnit.MINUTES"));
    }
}
