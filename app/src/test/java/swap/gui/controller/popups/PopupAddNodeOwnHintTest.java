package swap.gui.controller.popups;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public class PopupAddNodeOwnHintTest {
    @Test
    public void theMoneroBoxSaysAnOwnNodeIsTrustedAloneAndThatTrustingItIsNotEnough() {
        String hint = PopupAddNode.ownHintText("node");
        assertTrue(hint.contains("run yourself"));
        assertTrue(hint.contains("trusts it alone to confirm a seller's Monero lock"));
        assertFalse(hint.contains(".onion"));
    }

    @Test
    public void theElectrumBoxIsAboutPrivacyNotTheLockCheck() {
        String hint = PopupAddNode.ownHintText("server");
        assertTrue(hint.contains("never a public fallback"));
        assertFalse(hint.contains("Monero lock"));
    }
}
