package swap.listener.impl;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.util.HashSet;
import java.util.Set;
import org.junit.Test;

public class SwapListenerFinalityTest {
    @Test
    public void announcesOnlyWhenTheTargetIsReached() {
        Set<String> seen = new HashSet<>();
        assertFalse(SwapListenerImpl.announcesFinality(seen, "tx", 0));
        assertFalse(SwapListenerImpl.announcesFinality(seen, "tx", 9));
        assertTrue(SwapListenerImpl.announcesFinality(seen, "tx", 10));
    }

    @Test
    public void announcesOncePerTransactionEvenIfTheCountRepeatsOrGrows() {
        Set<String> seen = new HashSet<>();
        assertTrue(SwapListenerImpl.announcesFinality(seen, "tx", 10));
        assertFalse(SwapListenerImpl.announcesFinality(seen, "tx", 10));
        assertFalse(SwapListenerImpl.announcesFinality(seen, "tx", 11));
    }

    @Test
    public void aDifferentTransactionGetsItsOwnNotice() {
        Set<String> seen = new HashSet<>();
        assertTrue(SwapListenerImpl.announcesFinality(seen, "tx-a", 10));
        assertTrue(SwapListenerImpl.announcesFinality(seen, "tx-b", 10));
    }

    @Test
    public void theNoticeSaysWhatIsHappeningNext() {
        assertTrue(SwapListenerImpl.LOG_XMR_FINAL.contains("redeem signature"));
        assertTrue(SwapListenerImpl.LOG_XMR_FINAL.contains("redeems the Monero"));
    }
}
