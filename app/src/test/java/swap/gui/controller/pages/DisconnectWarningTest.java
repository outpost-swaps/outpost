package swap.gui.controller.pages;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/** Audit M-5: "Disconnect" warns whenever the Bitcoin may be locked. */
public class DisconnectWarningTest {
    @Test
    public void nothingRunningNeedsNoWarning() {
        assertFalse(DisconnectPolicy.needsWarning(false, 3, "awaiting confirmations"));
    }

    @Test
    public void beforeTheLockDisconnectingIsHarmless() {
        assertFalse(DisconnectPolicy.needsWarning(true, 1, "waiting for deposit"));
        assertFalse(DisconnectPolicy.needsWarning(true, 1, "deposit received"));
    }

    @Test
    public void fromNegotiationOnTheLockMayBeOut() {
        assertTrue(DisconnectPolicy.needsWarning(true, 1, "negotiated"));
        assertTrue(DisconnectPolicy.needsWarning(true, 1, "sent"));
        assertTrue(DisconnectPolicy.needsWarning(true, 2, "confirming"));
        assertTrue(DisconnectPolicy.needsWarning(true, 3, "awaiting confirmations"));
        assertTrue(DisconnectPolicy.needsWarning(true, 5, "sweeping"));
    }

    /** A resumed swap may not have reported a step yet: warn rather than assume. */
    @Test
    public void anUnknownStepWarns() {
        assertTrue(DisconnectPolicy.needsWarning(true, 0, null));
    }

    /** Source check: the button asks before stopping (the dialog needs a JavaFX stage). */
    @Test
    public void theDisconnectButtonAsksBeforeStopping() throws Exception {
        String source = new String(java.nio.file.Files.readAllBytes(java.nio.file.Paths.get(
                "src/main/java/swap/gui/controller/pages/SwapsController.java")));
        String body = source.substring(source.indexOf("public void onStopButtonClick()"));
        body = body.substring(0, body.indexOf("endSwapThread();") + 1);
        assertTrue(body.contains("DisconnectPolicy.needsWarning("));
        assertTrue(body.contains("confirmDisconnect("));
    }
}
