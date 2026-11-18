package swap.model;

import org.junit.Test;
import swap.listener.impl.SwapListenerImpl;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

/**
 * Audit 2026-11-10, H-2: a failure after the Bitcoin lock used to arrive as SwapSetupFailed,
 * which is not restarted, so the client stopped with Bitcoin locked and nothing watching the
 * timelocks. Such failures now arrive as SwapInterrupted, which is.
 */
public class SwapErrorTypeTest {

    @Test
    public void anInterruptedSwapIsRestarted() {
        assertTrue(SwapErrorType.SwapInterrupted.shouldRestart());
    }

    @Test
    public void aSetupFailureIsStillNotRestarted() {
        // nothing is locked before the lock; retrying the same seller only burns time
        assertFalse(SwapErrorType.SwapSetupFailed.shouldRestart());
    }

    @Test
    public void typesThatEndASwapForGoodAreNotRestarted() {
        assertFalse(SwapErrorType.SwapPunished.shouldRestart());
        assertFalse(SwapErrorType.SellerInsufficientBalance.shouldRestart());
    }

    @Test
    public void theNativeJsonParsesToSwapInterrupted() {
        // the shape src/util.rs post_lock_error produces
        SwapError error = SwapError.fromJson(
                "{\"swapId\":\"abc\",\"errorType\":\"SwapInterrupted\","
                        + "\"errorMessage\":\"Failed to cancel/refund Bitcoin: timeout\",\"fatal\":true}");
        assertEquals("abc", error.swapId());
        assertEquals(SwapErrorType.SwapInterrupted, error.errorType());
        assertTrue(error.fatal());
        assertTrue(error.errorType().shouldRestart());
    }

    @Test
    public void theStatusLineSaysTheSwapIsBeingResumed() {
        String text = SwapListenerImpl.restartDetail(true, 120);
        assertTrue(text, text.contains("resuming in 120s"));
        assertTrue(text, text.contains("Bitcoin may be locked"));
        assertFalse(text, text.contains("Setup failed"));
    }

    @Test
    public void aSetupRetryKeepsItsOwnStatusLine() {
        assertEquals("Setup failed; retrying in 60s", SwapListenerImpl.restartDetail(false, 60));
    }
}
