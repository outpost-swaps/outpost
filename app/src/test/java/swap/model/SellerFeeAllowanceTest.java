package swap.model;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import org.bitcoinj.params.TestNet3Params;
import org.junit.Test;
import swap.model.request.SwapRequest;

/** Matrix S-01 / WORKLOG §458: "accept and retry" for a seller asking more than our fee limit. */
public class SellerFeeAllowanceTest {
    // the live refusal: hermes-v5 asked 6,132 sat/kvB, our limit was 3,000, our estimate 1,000
    private static final String PEER = "/dns4/hermes-v5.eigenwallet.org/tcp/443/wss/p2p/12D3KooWDtaN";

    @Test
    public void itIsUsedOnceAndOnlyForThatSeller() {
        SellerFeeAllowance.allowOnce(PEER, 6132);
        assertNull("another seller does not get it", SellerFeeAllowance.take("/dns4/other/tcp/1/p2p/x"));
        // and asking for another seller used it up, so it cannot leak later
        assertNull(SellerFeeAllowance.take(PEER));

        SellerFeeAllowance.allowOnce(PEER, 6132);
        assertEquals("the asked rate; the engine adds the margin", Long.valueOf(6132), SellerFeeAllowance.take(PEER));
        assertNull("the next swap is protected again", SellerFeeAllowance.take(PEER));
    }

    @Test
    public void theDialogShowsTheRealNumbers() {
        assertEquals("6.1", SellerFeeAllowance.satPerVbyte(6132));
        // (6,132 - 1,000) sat/kvB on the 181-vB template = 929 sats per transaction, cancel + refund
        assertEquals(1858, SellerFeeAllowance.extraIfRefunded(6132, 1000));
        String summary = SellerFeeAllowance.summary(6132, 3000, 1000);
        assertTrue(summary, summary.contains("~6.1 sat/vB") && summary.contains("needs ~1.0 sat/vB") && summary.contains("up to ~3.0 sat/vB"));
        assertTrue(summary.contains("before any Bitcoin was locked"));
        assertTrue(SellerFeeAllowance.costNote(6132, 1000).contains("~1858 sats more"));
        assertEquals("Accept ~6.1 sat/vB and retry", SellerFeeAllowance.acceptLabel(6132));
        assertEquals("Try another seller", SellerFeeAllowance.declineLabel());
    }

    @Test
    public void theBuyRequestCarriesTheRateOnlyWhenAccepted() {
        SwapRequest request = new SwapRequest("id", "seed", "xmr", "tcp://e:1", PEER, "127.0.0.1:9050",
                TestNet3Params.get(), 9050, "tb1q", 1L, 0, 0, null, null);
        assertFalse(swap.client.ClientSwapRequests.buy(request, null).has("acceptedSellerFeeRateSatKvb"));
        assertEquals(6132, swap.client.ClientSwapRequests.buy(request, 6132L).getLong("acceptedSellerFeeRateSatKvb"));
    }

    /** The native call ([BJJJ)V (seller_fee::on_seller_fee_rate_too_high) must reach the String overload. */
    @Test
    public void theNativeCallbackReachesTheListener() throws Exception {
        java.lang.reflect.Method bytes = swap.listener.SwapListener.class
                .getMethod("onSellerFeeRateTooHigh", byte[].class, long.class, long.class, long.class);
        java.lang.reflect.Method strings = swap.listener.SwapListener.class
                .getMethod("onSellerFeeRateTooHigh", String.class, long.class, long.class, long.class);
        assertTrue(bytes.isDefault());
        assertTrue(java.lang.reflect.Modifier.isAbstract(strings.getModifiers()));
    }

    /** Source check: a new swap takes the allowance once; the buy request is built with it. */
    @Test
    public void aNewSwapTakesTheAllowanceOnce() throws Exception {
        String source = new String(java.nio.file.Files.readAllBytes(java.nio.file.Paths.get(
                "src/main/java/swap/client/ClientSwap.java")));
        assertEquals(1, source.split("SellerFeeAllowance.take\\(", -1).length - 1);
        assertTrue(source.contains("buyXmrV5(ClientSwapRequests.buy(swapRequest, acceptedSellerFeeRate).toString())"));
    }

    @Test
    public void worstRefundCostCoversTheLongestPath() {
        // 4 transactions of 181 vB at 6.132 sat/vB: 1109.9 rounds up to 1110 each
        assertEquals(4 * 1110, SellerFeeAllowance.worstRefundCost(6132));
    }

    @Test
    public void worstRefundTextShowsShareOfSwapWhenKnown() {
        assertEquals("~4,440 sats (0.44% of this swap)", SellerFeeAllowance.worstRefundText(6132, 1_000_000));
        assertEquals("~4,440 sats", SellerFeeAllowance.worstRefundText(6132, 0));
    }
}
