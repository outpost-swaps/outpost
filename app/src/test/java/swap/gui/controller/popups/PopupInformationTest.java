package swap.gui.controller.popups;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import java.io.InputStream;
import java.util.ArrayList;
import java.util.List;
import javax.xml.parsers.DocumentBuilderFactory;
import org.junit.Test;
import org.w3c.dom.Document;
import org.w3c.dom.Element;
import org.w3c.dom.NodeList;
import swap.model.Changelog;

public class PopupInformationTest {

    @Test
    public void bulletsAndNoteAreSeparated() {
        PopupInformation.Parsed parsed = PopupInformation.parse("- One\n- Two  \n\nConfused?\nWait.");
        assertEquals(List.of("One", "Two"), parsed.items());
        assertEquals("Confused? Wait.", parsed.note());
    }

    @Test
    public void emptyOrNullTextHasNothing() {
        assertTrue(PopupInformation.parse(null).items().isEmpty());
        assertEquals("", PopupInformation.parse("").note());
    }

    @Test
    public void theShippedChangelogHasBullets() {
        assertTrue(!PopupInformation.parse(Changelog.TEXT).items().isEmpty());
    }

    @Test
    public void everyFxIdHasAControllerField() throws Exception {
        List<String> missing = new ArrayList<>();
        try (InputStream in = getClass().getResourceAsStream("/popups/popup-information.fxml")) {
            Document doc = DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(in);
            NodeList all = doc.getElementsByTagName("*");
            for (int i = 0; i < all.getLength(); i++) {
                Element e = (Element) all.item(i);
                if (e.hasAttribute("fx:id")) {
                    try {
                        PopupInformation.class.getDeclaredField(e.getAttribute("fx:id"));
                    } catch (NoSuchFieldException ex) {
                        missing.add(e.getAttribute("fx:id"));
                    }
                }
            }
        }
        assertTrue("fx:id without a controller field: " + missing, missing.isEmpty());
    }
}
