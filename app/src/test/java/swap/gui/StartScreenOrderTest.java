package swap.gui;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.nio.file.Files;
import java.nio.file.Path;
import org.junit.Test;

/** The start screen must decide its layout before the window is shown, or the wrong layout flashes up first. */
public class StartScreenOrderTest {
    @Test
    public void theStartScreenIsSetUpBeforeTheWindowIsShown() throws Exception {
        String source = Files.readString(Path.of("src/main/java/swap/gui/GUISwap.java"));
        int init = source.indexOf(").init(stage);");
        int show = source.indexOf("stage.show();", source.indexOf("stage.setScene(pairingScene)"));

        assertTrue("init(stage) must be called", init > 0);
        assertTrue("stage.show() must follow it", show > 0);
        assertTrue("init(stage) must come before stage.show()", init < show);
    }

    @Test
    public void theLayoutFileDefaultsToTheUnlockLayoutSoNothingForeignShowsBeforeInit() throws Exception {
        String fxml = Files.readString(Path.of("src/main/resources/pairing-screen.fxml"));

        String flat = fxml.replaceAll("\\s+", " ");
        assertTrue(fxml.contains("fx:id=\"connectButton\"") && flat.contains("styleClass=\"dialog-button-danger, start-button-primary\" managed=\"false\" visible=\"false\""));
        assertTrue(fxml.contains("fx:id=\"createButton\"") && flat.contains("styleClass=\"dialog-button-secondary, start-button-secondary\" managed=\"false\" visible=\"false\""));
        // the stacked button group starts hidden too, and the one-line hint under the buttons is gone
        assertTrue(flat.contains("fx:id=\"startButtonsRow\" alignment=\"CENTER\" spacing=\"12.0\" managed=\"false\" visible=\"false\""));
        assertFalse(fxml.contains("connectHint"));
    }

    /**
     * The first screen (Connect wallet / Create new wallet) and the unlock screen must be the same height, with the
     * logo and the first control in the same place. So the start buttons live in the same slot as the unlock form,
     * and the form keeps its height when the buttons are shown (its controls are made invisible, not removed).
     */
    @Test
    public void theStartButtonsShareTheUnlockFormsSlotSoBothScreensAreTheSameHeight() throws Exception {
        String fxml = Files.readString(Path.of("src/main/resources/pairing-screen.fxml"));
        int slot = fxml.indexOf("<StackPane maxWidth=\"320.0\" alignment=\"TOP_CENTER\">");
        int slotEnd = fxml.indexOf("</StackPane>", slot);
        assertTrue("the slot is top-aligned", slot > 0 && slotEnd > slot);
        String inSlot = fxml.substring(slot, slotEnd);
        assertTrue("the unlock form is in the slot", inSlot.contains("fx:id=\"formBox\""));
        assertTrue("the start buttons are in the same slot", inSlot.contains("fx:id=\"startButtonsRow\""));
        assertTrue("so is the unlock progress", inSlot.contains("fx:id=\"stageBox\""));
        // the gap under the logo is the slot's own margin (20) plus the column spacing (20), the same on both screens
        assertFalse("no extra margin above the start buttons any more",
                fxml.substring(fxml.indexOf("fx:id=\"startButtonsRow\""), fxml.indexOf("fx:id=\"stageBox\"")).contains("Insets top"));

        String controller = Files.readString(Path.of("src/main/java/swap/gui/controller/PairingController.java"));
        assertTrue("the passphrase field is made invisible on the first screen, not removed from the layout",
                controller.contains("walletPassphraseTextField.setVisible(returning);"));
        assertTrue(controller.contains("pairRow.setVisible(returning);"));
        assertFalse("removing them from the layout would make the first screen shorter than the unlock screen",
                controller.contains("show(walletPassphraseTextField, returning)") || controller.contains("show(pairRow, returning)"));
    }
}
