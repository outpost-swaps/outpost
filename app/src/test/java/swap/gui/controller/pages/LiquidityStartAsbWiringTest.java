package swap.gui.controller.pages;

import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertTrue;

import java.io.InputStream;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import org.junit.Test;

/** The Liquidity page's Start ASB button: the layout names it, and the controller has the field and handler. */
public class LiquidityStartAsbWiringTest {
    @Test
    public void theButtonAndItsHandlerExist() throws Exception {
        String fxml;
        try (InputStream in = LiquidityStartAsbWiringTest.class.getResourceAsStream("/pages/liquidity.fxml")) {
            assertNotNull(in);
            fxml = new String(in.readAllBytes(), StandardCharsets.UTF_8);
        }
        assertTrue(fxml.contains("fx:id=\"startAsbButton\""));
        assertTrue(fxml.contains("onAction=\"#onStartAsbClick\""));

        assertNotNull(LiquidityController.class.getField("startAsbButton"));
        boolean handler = false;
        for (Method method : LiquidityController.class.getDeclaredMethods()) {
            handler |= method.getName().equals("onStartAsbClick");
        }
        assertTrue(handler);
    }
}
