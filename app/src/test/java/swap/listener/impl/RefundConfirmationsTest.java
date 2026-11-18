package swap.listener.impl;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public class RefundConfirmationsTest {
    @Test
    public void protocol5CancelTimelocksAreShown() {
        // mainnet: 30-block cancel timelock, testnet4: 42 (not the old protocol's 72 and 12)
        assertEquals(29, SwapListenerImpl.refundConfirmationsAfterLock(true));
        assertEquals(41, SwapListenerImpl.refundConfirmationsAfterLock(false));
        // the line printed when the lock goes out names the whole timelock
        assertEquals(30, SwapListenerImpl.refundTimelockBlocks(true));
        assertEquals(42, SwapListenerImpl.refundTimelockBlocks(false));
    }
}
