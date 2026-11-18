package swap.runnable;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.nio.file.Files;
import java.nio.file.Path;
import org.junit.Test;

/** Pairing needs Tor (it downloads the wallet tools and opens the wallet through it), so the buttons wait for Tor. */
public class PairingGateTest {
    @Test
    public void withTorInUsePairingWaitsForTor() {
        assertFalse(RunnableStartButtonChecker.pairingEnabled(true, false, true));
        assertTrue(RunnableStartButtonChecker.pairingEnabled(true, true, true));
    }

    @Test
    public void nothingCanBeStartedWhileAPairingIsAlreadyRunning() {
        assertFalse(RunnableStartButtonChecker.pairingEnabled(true, true, false));
        assertFalse(RunnableStartButtonChecker.pairingEnabled(false, false, false));
    }

    @Test
    public void withoutTorTheOnlyConditionIsThatNoPairingRuns() {
        assertTrue(RunnableStartButtonChecker.pairingEnabled(false, false, true));
    }

    @Test
    public void connectAndCreateFollowTheSameGateAsPair() throws Exception {
        String source = Files.readString(Path.of("src/main/java/swap/gui/controller/PairingController.java"));
        assertTrue("Connect and Create must be handed to the Tor check",
                source.contains("getExtraStartButtons()") && source.contains("List.of(connectButton, createButton)"));
        String base = Files.readString(Path.of("src/main/java/swap/gui/controller/BaseController.java"));
        assertTrue("the base controller must pass them to the checker", base.contains("getStopButton(), getExtraStartButtons())"));
    }
}
