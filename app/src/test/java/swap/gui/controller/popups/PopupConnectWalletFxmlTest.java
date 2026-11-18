package swap.gui.controller.popups;

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

/**
 * The windows cannot be loaded without a JavaFX toolkit, so this checks what a unit test can see:
 * every {@code fx:id} has a controller field and every {@code onAction} a method (a mismatch is a
 * window that fails to open).
 */
public class PopupConnectWalletFxmlTest {
    private static Document parse(String resource) throws Exception {
        try (InputStream in = PopupConnectWalletFxmlTest.class.getResourceAsStream(resource)) {
            assertNotNull(resource + " must be on the classpath", in);
            return DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(in);
        }
    }

    private static List<String> values(Document doc, String attribute) {
        List<String> out = new ArrayList<>();
        NodeList all = doc.getElementsByTagName("*");
        for (int i = 0; i < all.getLength(); i++) {
            Element element = (Element) all.item(i);
            if (element.hasAttribute(attribute)) {
                out.add(element.getAttribute(attribute));
            }
        }
        return out;
    }

    private static void check(String resource, Class<?> controller) throws Exception {
        Document doc = parse(resource);
        List<String> missing = new ArrayList<>();
        for (String id : values(doc, "fx:id")) {
            try {
                controller.getDeclaredField(id);
            } catch (NoSuchFieldException e) {
                try {
                    controller.getField(id);
                } catch (NoSuchFieldException again) {
                    missing.add(id);
                }
            }
        }
        for (String handler : values(doc, "onAction")) {
            String name = handler.startsWith("#") ? handler.substring(1) : handler;
            boolean found = false;
            for (Method method : controller.getDeclaredMethods()) {
                found |= method.getName().equals(name);
            }
            if (!found) {
                missing.add("onAction " + name);
            }
        }
        assertTrue(resource + ": no controller member for " + missing, missing.isEmpty());
    }

    @Test
    public void everyFxIdAndHandlerOfTheConnectWindowExists() throws Exception {
        check("/popups/popup-connect-wallet.fxml", PopupConnectWallet.class);
    }

    @Test
    public void everyFxIdAndHandlerOfTheChangeDojoWindowExists() throws Exception {
        check("/popups/popup-change-dojo.fxml", PopupChangeDojo.class);
    }

    @Test
    public void thePairingScreenIsWired() throws Exception {
        check("/pairing-screen.fxml", swap.gui.controller.PairingController.class);
    }
}
