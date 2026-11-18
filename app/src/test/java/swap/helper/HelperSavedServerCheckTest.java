package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;

import org.junit.Test;

public class HelperSavedServerCheckTest {
    @Test
    public void aServerThatPassedGivesNoWarning() {
        assertNull(HelperSavedServerCheck.warning("Electrum server", "tcp://a.onion:50001", true, null));
    }

    @Test
    public void aFailedServerIsNamedWithTheReasonAndSettings() {
        assertEquals("Your saved Electrum server tcp://a.onion:50001 did not pass a check (timed out). Change it in Settings.",
                HelperSavedServerCheck.warning("Electrum server", "tcp://a.onion:50001", false, "timed out"));
    }

    @Test
    public void aMissingReasonStillReads() {
        assertEquals("Your saved Monero node http://n.onion:18089 did not pass a check (no answer). Change it in Settings.",
                HelperSavedServerCheck.warning("Monero node", "http://n.onion:18089", false, " "));
    }
}
