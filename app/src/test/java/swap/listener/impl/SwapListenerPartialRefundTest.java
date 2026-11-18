package swap.listener.impl;

import static org.junit.Assert.assertTrue;

import org.junit.Test;

public class SwapListenerPartialRefundTest {
    @Test
    public void theLogSaysWhatCameBackAndWhatTheSellerKept() {
        String line = SwapListenerImpl.partialRefundLog("abc", 490_000, 10_000);
        assertTrue(line, line.contains("partially refunded"));
        assertTrue(line, line.contains("490000 sats came back"));
        assertTrue(line, line.contains("kept its 10000 sat deposit"));
    }

    /** The native call ([BJJ)V must reach the String overload. */
    @Test
    public void theNativeCallbackReachesTheListener() throws Exception {
        java.lang.reflect.Method bytes = swap.listener.SwapListener.class
                .getMethod("onSwapPartiallyRefunded", byte[].class, long.class, long.class);
        java.lang.reflect.Method strings = swap.listener.SwapListener.class
                .getMethod("onSwapPartiallyRefunded", String.class, long.class, long.class);
        assertTrue(bytes.isDefault());
        assertTrue(java.lang.reflect.Modifier.isAbstract(strings.getModifiers()));
    }
}
