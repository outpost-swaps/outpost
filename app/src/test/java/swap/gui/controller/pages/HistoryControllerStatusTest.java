package swap.gui.controller.pages;

import org.junit.Test;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotEquals;

public class HistoryControllerStatusTest {

    @Test
    public void legacyStatusesRemainPrettified() {
        assertEquals("BTC locked", HistoryController.prettifyAsbStatus("btc is locked"));
        assertEquals("Cancelled", HistoryController.prettifyAsbStatus("btc is cancelled"));
        assertEquals("BTC refunded", HistoryController.prettifyAsbStatus("btc is refunded"));
    }

    @Test
    public void v5StatusesArePrettified() {
        assertEquals(
                "Cancelled",
                HistoryController.prettifyAsbStatus("Bitcoin cancel transaction published"));
        assertEquals(
                "XMR refunded",
                HistoryController.prettifyAsbStatus("Monero refund transaction published"));
        assertEquals("Punished", HistoryController.prettifyAsbStatus("Done: BtcPunished"));
        assertEquals("BTC locked", HistoryController.prettifyAsbStatus("Bitcoin locked"));
        assertEquals("BTC redeemed", HistoryController.prettifyAsbStatus("Done: BtcRedeemed"));
        assertEquals("XMR locked", HistoryController.prettifyAsbStatus("Monero locked"));
    }

    @Test
    public void v5StatusesAreCanonicalForPieCounts() {
        assertEquals(
                "btc is punished",
                HistoryController.canonicalAsbStatus("Done: BtcPunished"));
        assertEquals(
                "btc is cancelled",
                HistoryController.canonicalAsbStatus("Bitcoin cancel transaction published"));
        assertEquals(
                "xmr is refunded",
                HistoryController.canonicalAsbStatus("Monero refund transaction published"));
        assertEquals(
                "btc is locked",
                HistoryController.canonicalAsbStatus("Bitcoin locked"));
        assertEquals(
                "btc is redeemed",
                HistoryController.canonicalAsbStatus("Done: BtcRedeemed"));
    }

    /** A seller kept the deposit: shown as such, counted, and never resumed. */
    @Test
    public void aKeptDepositIsShownAsPartiallyRefunded() {
        assertEquals("Partially refunded", HistoryController.prettifySwapStatus("PARTIALLY_REFUNDED"));
        assertNotEquals("Refunded", HistoryController.prettifySwapStatus("PARTIALLY_REFUNDED"));
        assertEquals(swap.model.SwapData.Status.PARTIALLY_REFUNDED,
                swap.model.SwapData.Status.valueOf("PARTIALLY_REFUNDED"));
        org.junit.Assert.assertTrue(HistoryController.isNonResumableStatus("PARTIALLY_REFUNDED"));
        org.junit.Assert.assertFalse(HistoryController.isNonResumableStatus("REFUND_PENDING"));
    }

    @Test
    public void pendingRefundIsNotShownAsCompletedOrCancelled() {
        assertEquals("BTC locked", HistoryController.prettifySwapStatus("BTC_LOCKED"));
        assertEquals("Refund pending", HistoryController.prettifySwapStatus("REFUND_PENDING"));
        assertNotEquals("Refunded", HistoryController.prettifySwapStatus("REFUND_PENDING"));
        assertNotEquals("Cancelled", HistoryController.prettifySwapStatus("REFUND_PENDING"));
        assertEquals("Refunded", HistoryController.prettifySwapStatus("REFUNDED"));
        assertEquals("Cancelled", HistoryController.prettifySwapStatus("CANCELLED"));
    }
}
