package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public class HelperSwapEstimateTest {
    private static final Double MIN = 0.001;
    private static final Double MAX = 0.05;
    private static final Double PRICE = 0.0078;

    @Test
    public void emptyOrBadAmountStaysQuiet() {
        HelperSwapEstimate.Conversion c = HelperSwapEstimate.fromBtc("", MIN, MAX, PRICE);
        assertNull(c.btc());
        assertNull(c.xmr());
        assertTrue(c.note().isEmpty());
        assertNull(HelperSwapEstimate.fromBtc("abc", MIN, MAX, PRICE).btc());
        assertNull(HelperSwapEstimate.fromBtc("-1", MIN, MAX, PRICE).btc());
    }

    @Test
    public void noSellerSaysSo() {
        HelperSwapEstimate.Conversion c = HelperSwapEstimate.fromBtc("0.01", null, null, null);
        assertEquals(HelperSwapEstimate.NO_SELLER, c.note());
        assertNull(c.xmr());
    }

    @Test
    public void outsideTheSellersRangeIsNamed() {
        assertEquals(HelperSwapEstimate.BELOW_MIN, HelperSwapEstimate.fromBtc("0.0005", MIN, MAX, PRICE).note());
        assertEquals(HelperSwapEstimate.ABOVE_MAX, HelperSwapEstimate.fromBtc("0.06", MIN, MAX, PRICE).note());
    }

    @Test
    public void btcToXmrSubtractsBothFees() {
        // (0.0156 - 0.00001) / 0.0078 - 0.00005 = 1.9987...
        HelperSwapEstimate.Conversion c = HelperSwapEstimate.fromBtc("0.0156", MIN, MAX, PRICE);
        assertEquals("1.998668", HelperSwapEstimate.formatXmr(c.xmr()));
    }

    @Test
    public void xmrToBtcAddsBothFees() {
        // (1.9987 + 0.00005) * 0.0078 + 0.00001 = 0.0156 (to the satoshi)
        HelperSwapEstimate.Conversion c = HelperSwapEstimate.fromXmr("1.9987", MIN, MAX, PRICE);
        assertEquals(0.0156, c.btc(), 0.0000005);
    }

    @Test
    public void theTwoDirectionsRoundTrip() {
        HelperSwapEstimate.Conversion forward = HelperSwapEstimate.fromBtc("0.02", MIN, MAX, PRICE);
        HelperSwapEstimate.Conversion back =
                HelperSwapEstimate.fromXmr(HelperSwapEstimate.formatXmr(forward.xmr()), MIN, MAX, PRICE);
        // The XMR side is shown with four decimals, so the return trip lands within one satoshi.
        assertEquals(0.02, back.btc(), 0.00000001);
    }

    @Test
    public void xmrAboveTheRangeIsNamed() {
        // 10 XMR costs far more than the 0.05 BTC ceiling.
        assertEquals(HelperSwapEstimate.ABOVE_MAX, HelperSwapEstimate.fromXmr("10", MIN, MAX, PRICE).note());
    }

    @Test
    public void commaDecimalsAreAccepted() {
        assertEquals("1.998668", HelperSwapEstimate.formatXmr(
                HelperSwapEstimate.fromBtc("0,0156", MIN, MAX, PRICE).xmr()));
    }

    @Test
    public void formattingTrimsButKeepsPrecision() {
        assertEquals("0.0156", HelperSwapEstimate.formatBtc(0.0156));
        assertEquals("0.00000001", HelperSwapEstimate.formatBtc(0.00000001));
        assertEquals("", HelperSwapEstimate.formatBtc(null));
        assertEquals("", HelperSwapEstimate.formatXmr(0.0));
        assertEquals("1.250000", HelperSwapEstimate.formatXmr(1.25));
    }

    @Test
    public void theSellerFieldStandsInWhenTheTableSelectionIsCleared() {
        swap.model.Seller a = new swap.model.Seller("/onion3/aaa:9939/p2p/12D3A", 0.001, 0.05, 0.0078, "100%");
        swap.model.Seller b = new swap.model.Seller("/dns4/b.example/tcp/443/wss/p2p/12D3B", 0.002, 0.04, 0.0079, "90%");
        java.util.List<swap.model.Seller> all = java.util.List.of(a, b);
        assertEquals(b, HelperSwapEstimate.sellerFor(b, "", all));
        assertEquals(a, HelperSwapEstimate.sellerFor(null, "  /onion3/aaa:9939/p2p/12D3A ", all));
        assertNull(HelperSwapEstimate.sellerFor(null, "/onion3/unknown", all));
        assertNull(HelperSwapEstimate.sellerFor(null, null, all));
    }
}
