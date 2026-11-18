package swap.gui.controller.pages;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import java.nio.file.Files;
import java.nio.file.Paths;
import org.junit.Test;

/** Matrix N-15: the exact XMR received on the Swap Success screen. */
public class SwapReceivedTest {
    // the real stagenet sweep the engine test decodes: 0.1253 XMR locked, fee 121,760,000 piconero
    private static final long RECEIVED = 125_178_240_000L;
    private static final long FEE = 121_760_000L;

    @Test
    public void amountsAreExactWithNoTrailingZeros() {
        assertEquals("0.12517824", SwapReceived.xmr(RECEIVED));
        assertEquals("0.00012176", SwapReceived.xmr(FEE));
        assertEquals("0.000000000001", SwapReceived.xmr(1));
        assertEquals("1", SwapReceived.xmr(1_000_000_000_000L));
        assertEquals("0", SwapReceived.xmr(0));
    }

    @Test
    public void theScreenSaysWhatArrivedAndWhatTheNetworkTook() {
        assertEquals("0.12517824 XMR", SwapReceived.amountLine(RECEIVED));
        assertEquals("Network fee: 0.00012176 XMR", SwapReceived.caption(FEE));
        assertEquals("Received 0.12517824 XMR (network fee 0.00012176 XMR).", SwapReceived.logLine(RECEIVED, FEE));
    }

    @Test(expected = IllegalArgumentException.class)
    public void aNegativeAmountIsRefusedNotShown() {
        SwapReceived.xmr(-1);
    }

    /** The native call ([BJJ)V (swap::util::on_xmr_received) must reach the String overload. */
    @Test
    public void theNativeCallbackReachesTheListener() throws Exception {
        java.lang.reflect.Method bytes = swap.listener.SwapListener.class
                .getMethod("onXmrReceived", byte[].class, long.class, long.class);
        java.lang.reflect.Method strings = swap.listener.SwapListener.class
                .getMethod("onXmrReceived", String.class, long.class, long.class);
        assertTrue(bytes.isDefault());
        assertTrue(java.lang.reflect.Modifier.isAbstract(strings.getModifiers()));
    }

    /** Layout: the amount sits inside the status card, between "Swap Success" and the swap ID. */
    @Test
    public void theAmountSitsInsideTheSuccessCard() throws Exception {
        String fxml = new String(Files.readAllBytes(Paths.get("src/main/resources/pages/swaps.fxml")));
        int card = fxml.indexOf("fx:id=\"swapStatusBox\"");
        int headline = fxml.indexOf("fx:id=\"swapStatusText\"");
        int box = fxml.indexOf("fx:id=\"swapReceivedBox\"");
        int detail = fxml.indexOf("fx:id=\"swapStatusDetail\"");
        assertTrue(card > 0 && card < headline && headline < box && box < detail);
        assertTrue(fxml.contains("fx:id=\"swapReceivedAmount\""));
        assertTrue(fxml.contains("fx:id=\"swapReceivedCaption\""));
    }

    /** The card grows by exactly the bar slot it replaces (22 + 6 + 18 px), so nothing below moves. */
    @Test
    public void theCardGrowsByTheBarSlotItReplaces() throws Exception {
        assertEquals(SwapsController.STATUS_CARD_HEIGHT + 22 + 6 + 18,
                SwapsController.STATUS_CARD_WITH_AMOUNT_HEIGHT, 0.0);
        String fxml = new String(Files.readAllBytes(Paths.get("src/main/resources/pages/swaps.fxml")));
        assertTrue(fxml.contains("minHeight=\"22.0\" prefHeight=\"22.0\""));
        assertTrue(fxml.contains("<Insets bottom=\"18.0\" top=\"6.0\" />"));
        assertTrue(fxml.contains("minHeight=\"54.0\" prefHeight=\"54.0\""));
    }

    /** Source check: success shows the amount only when the engine reported one for this swap. */
    @Test
    public void successShowsOnlyAReportedAmount() throws Exception {
        String source = new String(Files.readAllBytes(Paths.get(
                "src/main/java/swap/listener/impl/SwapListenerImpl.java")));
        int completed = source.indexOf("public void onSwapCompleted(");
        int end = source.indexOf("\n    }\n", completed);
        String body = source.substring(completed, end);
        assertTrue(body.contains("xmrReceived.remove(completedSwap.swapId())"));
        assertTrue(body.contains("if (received != null) swapsController.showXmrReceived("));
    }
}
