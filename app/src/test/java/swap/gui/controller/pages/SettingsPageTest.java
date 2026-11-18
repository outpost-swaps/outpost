package swap.gui.controller.pages;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertTrue;

import java.io.InputStream;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.List;
import javax.xml.parsers.DocumentBuilderFactory;
import org.junit.Test;
import org.w3c.dom.Document;
import org.w3c.dom.Element;
import org.w3c.dom.NodeList;

/**
 * The settings page cannot be loaded without a JavaFX toolkit, so this checks the ways loading it
 * fails that a unit test can see: an {@code fx:id} with no controller field, and an
 * {@code onAction} handler with no method. (A page that fails to load is a blank settings page.)
 */
public class SettingsPageTest {

    private static Document page() throws Exception {
        try (InputStream in = SettingsPageTest.class.getResourceAsStream("/pages/settings.fxml")) {
            assertNotNull("settings.fxml must be on the classpath", in);
            return DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(in);
        }
    }

    private static List<String> attributeValues(Document doc, String attribute) {
        List<String> values = new ArrayList<>();
        NodeList all = doc.getElementsByTagName("*");
        for (int i = 0; i < all.getLength(); i++) {
            Element element = (Element) all.item(i);
            if (element.hasAttribute(attribute)) {
                values.add(element.getAttribute(attribute));
            }
        }
        return values;
    }

    private static Class<?> controller() throws Exception {
        return Class.forName(
                "swap.gui.controller.pages.SettingsController", false, SettingsPageTest.class.getClassLoader());
    }

    @Test
    public void everyFxIdHasAControllerField() throws Exception {
        List<String> missing = new ArrayList<>();
        for (String id : attributeValues(page(), "fx:id")) {
            try {
                Field field = controller().getDeclaredField(id);
                assertNotNull(field);
            } catch (NoSuchFieldException e) {
                missing.add(id);
            }
        }
        assertTrue("fx:id without a controller field: " + missing, missing.isEmpty());
    }

    @Test
    public void everyOnActionHandlerExists() throws Exception {
        List<String> missing = new ArrayList<>();
        for (String handler : attributeValues(page(), "onAction")) {
            String name = handler.startsWith("#") ? handler.substring(1) : handler;
            boolean found = false;
            for (Method method : controller().getDeclaredMethods()) {
                found |= method.getName().equals(name);
            }
            if (!found) {
                missing.add(name);
            }
        }
        assertTrue("onAction without a controller method: " + missing, missing.isEmpty());
    }

    @Test
    public void theOwnNodeCheckboxIsOnThePage() throws Exception {
        assertTrue(attributeValues(page(), "fx:id").contains("ownMoneroNode"));
        assertNotNull(controller().getDeclaredField("ownMoneroNode"));
    }

    @Test
    public void nodeTypeIsOneChoiceBetweenTwoRadioButtons() throws Exception {
        List<String> ids = attributeValues(page(), "fx:id");
        assertTrue(ids.contains("ownMoneroNode"));
        assertTrue(ids.contains("publicMoneroNode"));
        assertTrue(ids.contains("moneroNodeType"));
        assertEquals(javafx.scene.control.RadioButton.class, controller().getDeclaredField("ownMoneroNode").getType());
    }

    @Test
    public void anOwnNodeShowsPinnedAndTrustedWithoutTouchingTheSavedChoices() {
        assertArrayEquals(new boolean[] {false, true}, SettingsController.shownNodeOptions(true, true, false));
        assertArrayEquals(new boolean[] {false, true}, SettingsController.shownNodeOptions(true, false, true));
        assertArrayEquals(new boolean[] {true, false}, SettingsController.shownNodeOptions(false, true, false));
        assertArrayEquals(new boolean[] {false, false}, SettingsController.shownNodeOptions(false, false, false));
    }

    @Test
    public void anOwnElectrumServerShowsFailoverOffWithoutTouchingTheSavedChoice() {
        assertFalse(SettingsController.shownElectrumFailover(true, true));
        assertTrue(SettingsController.shownElectrumFailover(false, true));
        assertFalse(SettingsController.shownElectrumFailover(false, false));
    }

    /**
     * The settings cards split on the same centre line as the Withdraw page: two 50% columns, the
     * same gap, and no one-sided padding around the grid (it shifted the gap 5 px left of centre).
     */
    @Test
    public void theCardColumnsSplitOnTheSameCentreLineAsWithdraw() throws Exception {
        Document settings = page();
        Element scroll = (Element) settings.getElementsByTagName("ScrollPane").item(0);
        NodeList insets = scroll.getElementsByTagName("Insets");
        for (int i = 0; i < insets.getLength(); i++) {
            Element inset = (Element) insets.item(i);
            if (inset.getParentNode().getParentNode() == scroll) {
                assertEquals("left and right padding around the card grid must match",
                        inset.getAttribute("left"), inset.getAttribute("right"));
            }
        }
        Element grid = (Element) scroll.getElementsByTagName("GridPane").item(0);
        Document withdraw;
        try (InputStream in = SettingsPageTest.class.getResourceAsStream("/pages/withdraw.fxml")) {
            withdraw = DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(in);
        }
        Element withdrawGrid = (Element) withdraw.getElementsByTagName("GridPane").item(0);
        assertEquals(Double.parseDouble(withdrawGrid.getAttribute("hgap")),
                Double.parseDouble(grid.getAttribute("hgap")), 0.0);
        NodeList columns = grid.getElementsByTagName("ColumnConstraints");
        assertEquals("50.0", ((Element) columns.item(0)).getAttribute("percentWidth"));
        assertEquals("50.0", ((Element) columns.item(1)).getAttribute("percentWidth"));

        String css;
        try (InputStream in = SettingsPageTest.class.getResourceAsStream("/theme.css")) {
            css = new String(in.readAllBytes(), java.nio.charset.StandardCharsets.UTF_8);
        }
        int rule = css.indexOf(".settings-scroll {");
        String body = css.substring(rule, css.indexOf('}', rule));
        assertFalse("the settings scroll pane must not pad one side only", body.contains("-fx-padding: 0 10 0 0"));
    }
}
