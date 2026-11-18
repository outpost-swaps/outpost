package swap.listener.impl;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.After;
import org.junit.Test;
import swap.model.UserDisconnect;

public class SwapListenerRetryPolicyTest {
    @After
    public void clean() {
        UserDisconnect.clear();
    }

    @Test
    public void aSwapTheUserCancelledBeforeTheLockIsNeverRetried() {
        assertFalse(SwapListenerImpl.shouldRetry(false, true));
        assertTrue(SwapListenerImpl.shouldRetry(false, false));
    }

    @Test
    public void afterTheLockAFailureAlwaysResumes() {
        assertTrue(SwapListenerImpl.shouldRetry(true, true));
        assertTrue(SwapListenerImpl.shouldRetry(true, false));
    }

    @Test
    public void disconnectingIsRememberedUntilANewSwapActionStarts() {
        assertFalse(UserDisconnect.happened());
        UserDisconnect.note(false);
        assertTrue(UserDisconnect.happened());
        assertTrue(UserDisconnect.knownSafe());
        UserDisconnect.clear();
        assertFalse(UserDisconnect.happened());
        assertFalse(UserDisconnect.knownSafe());
    }

    @Test
    public void aDisconnectWithBitcoinPossiblyLockedIsNeverCalledSafe() {
        UserDisconnect.note(true);
        assertTrue(UserDisconnect.happened());
        assertFalse(UserDisconnect.knownSafe());
    }

    @Test
    public void aPreDepositCancelSaysNothingNeedsResuming() {
        assertEquals("Disconnected. Nothing was locked, so there is nothing to resume.",
                SwapListenerImpl.disconnectMessage(true));
        assertTrue(SwapListenerImpl.disconnectMessage(false).contains("risk being punished"));
    }
}
