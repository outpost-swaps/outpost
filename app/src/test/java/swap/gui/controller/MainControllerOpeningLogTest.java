package swap.gui.controller;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public class MainControllerOpeningLogTest {
    @Test
    public void theDownloadOnTheStartScreenIsLoggedWithItsLength() {
        assertEquals("Downloaded and verified the Monero wallet tools in 2 min 6 s (first launch only).",
                MainController.rpcDownloadLogLine(126_000));
        assertEquals("Downloaded and verified the Monero wallet tools in 42 s (first launch only).",
                MainController.rpcDownloadLogLine(42_000));
    }
}
