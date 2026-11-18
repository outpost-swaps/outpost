package swap.gui.controller.popups;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertTrue;

import java.io.InputStream;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.List;
import javax.xml.parsers.DocumentBuilderFactory;
import org.junit.Test;
import org.w3c.dom.Document;
import org.w3c.dom.Element;
import org.w3c.dom.NodeList;

/** The Add Node window: its FXML matches the controller, and Save only opens for usable input. */
public class PopupAddNodeTest {

    private static Document page() throws Exception {
        try (InputStream in = PopupAddNodeTest.class.getResourceAsStream("/popups/popup-add-node.fxml")) {
            assertNotNull("popup-add-node.fxml must be on the classpath", in);
            return DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(in);
        }
    }

    private static List<String> values(String attribute) throws Exception {
        List<String> out = new ArrayList<>();
        NodeList all = page().getElementsByTagName("*");
        for (int i = 0; i < all.getLength(); i++) {
            Element e = (Element) all.item(i);
            if (e.hasAttribute(attribute)) {
                out.add(e.getAttribute(attribute));
            }
        }
        return out;
    }

    @Test
    public void everyFxIdHasAControllerField() throws Exception {
        List<String> missing = new ArrayList<>();
        for (String id : values("fx:id")) {
            try {
                PopupAddNode.class.getDeclaredField(id);
            } catch (NoSuchFieldException e) {
                missing.add(id);
            }
        }
        assertTrue("fx:id without a controller field: " + missing, missing.isEmpty());
    }

    @Test
    public void everyOnActionHandlerExists() throws Exception {
        List<String> missing = new ArrayList<>();
        for (String handler : values("onAction")) {
            String name = handler.startsWith("#") ? handler.substring(1) : handler;
            boolean found = false;
            for (Method m : PopupAddNode.class.getDeclaredMethods()) {
                found |= m.getName().equals(name);
            }
            if (!found) {
                missing.add(name);
            }
        }
        assertTrue("onAction without a method: " + missing, missing.isEmpty());
    }

    @Test
    public void saveNeedsAnAddressAndAValidPortWhereAskedFor() {
        assertTrue(PopupAddNode.inputValid("tcp://host", "50001", true));
        assertTrue(PopupAddNode.inputValid(" tcp://host ", " 50001 ", true));
        assertFalse(PopupAddNode.inputValid("", "50001", true));
        assertFalse(PopupAddNode.inputValid(null, "50001", true));
        assertFalse(PopupAddNode.inputValid("tcp://host", "", true));
        assertFalse(PopupAddNode.inputValid("tcp://host", "abc", true));
        assertFalse(PopupAddNode.inputValid("tcp://host", "0", true));
        assertFalse(PopupAddNode.inputValid("tcp://host", "70000", true));
    }

    @Test
    public void aPeerWithoutAPortOnlyNeedsAnAddress() {
        assertTrue(PopupAddNode.inputValid("/ip4/1.2.3.4/tcp/9939", "", false));
        assertFalse(PopupAddNode.inputValid("  ", "", false));
    }
}
