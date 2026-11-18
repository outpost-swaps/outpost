package swap.gui.controller;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public class PairingStagesTest {
    @Test
    public void theStepListMarksDoneCurrentAndPending() {
        assertEquals("\u2713  Checking your passphrase", PairingController.stageLine(0, 2));
        assertEquals("\u2713  Setting up your wallet", PairingController.stageLine(1, 2));
        assertEquals("\u25CF  Downloading Monero wallet files", PairingController.stageLine(2, 2));
        assertEquals("\u25CB  Opening your wallet", PairingController.stageLine(3, 2));
    }

    @Test
    public void everyStepHasText() {
        for (int i = 0; i < 4; i++) {
            assertTrue(PairingController.stageLine(i, 0).length() > 4);
        }
    }

    @Test
    public void theDownloadStepShowsItsPercentageOnlyWhileItIsTheCurrentStep() {
        assertEquals("\u25CF  Downloading Monero wallet files  \u00B7  35%", PairingController.stageLine(2, 2, 35));
        assertEquals("\u25CF  Downloading Monero wallet files", PairingController.stageLine(2, 2, -1));
        assertEquals("\u2713  Downloading Monero wallet files", PairingController.stageLine(2, 3, 100));
        assertEquals("\u25CB  Opening your wallet", PairingController.stageLine(3, 2, 35));
    }

    @Test
    public void anUnlockStaysOnTheUnlockScreenUnlessADownloadIsReallyHappening() {
        // returning user: no list while unlocking...
        assertFalse(PairingController.showsStepList(false, false, 0));
        assertFalse(PairingController.showsStepList(false, false, 1));
        assertFalse(PairingController.showsStepList(false, false, 3));
        // ...but a real download brings it up
        assertTrue(PairingController.showsStepList(false, true, 2));
        // a first connect always shows it, and nothing shows when it is switched off
        assertTrue(PairingController.showsStepList(true, false, 0));
        assertFalse(PairingController.showsStepList(true, true, -1));
    }
}
