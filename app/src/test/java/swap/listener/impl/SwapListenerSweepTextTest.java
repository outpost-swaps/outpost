package swap.listener.impl;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public class SwapListenerSweepTextTest {
    private static final String TXID = "ab12cd34";

    @Test
    public void theBroadcastNoticeShowsTheTransactionAndSaysItIsWaitingForConfirmation() {
        assertEquals(
                "Monero sweep broadcast in transaction ab12cd34. Waiting for confirmation...",
                SwapListenerImpl.sweepBroadcastNotice(TXID));
    }

    @Test
    public void theWaitLineSaysHowLongItHasBeenInMinutes() {
        assertEquals("Still waiting for the Monero sweep to be confirmed (2m)...", SwapListenerImpl.sweepWaitLog(120));
        assertEquals("Still waiting for the Monero sweep to be confirmed (4m)...", SwapListenerImpl.sweepWaitLog(240));
        assertEquals("Sweep broadcast - waiting for confirmation (2m)", SwapListenerImpl.sweepWaitStatus(120));
    }

    /** Ticks, not the clock: the sweep heartbeat is exactly 2m, 4m, 6m (one Monero block apart). */
    @Test
    public void theSweepHeartbeatCountsWholeBlocks() {
        long period = SwapListenerImpl.SWEEP_HEARTBEAT_SECONDS;
        assertEquals("2m", SwapListenerImpl.elapsed(SwapListenerImpl.heartbeatElapsed(1, period)));
        assertEquals("4m", SwapListenerImpl.elapsed(SwapListenerImpl.heartbeatElapsed(2, period)));
        assertEquals("6m", SwapListenerImpl.elapsed(SwapListenerImpl.heartbeatElapsed(3, period)));
        // the 30 s redeem-preparation heartbeat
        assertEquals("1m 30s", SwapListenerImpl.elapsed(SwapListenerImpl.heartbeatElapsed(3, SwapListenerImpl.SYNC_HEARTBEAT_SECONDS)));
    }

    @Test
    public void elapsedTimeReadsInSecondsThenMinutes() {
        assertEquals("40s", SwapListenerImpl.elapsed(40));
        assertEquals("59s", SwapListenerImpl.elapsed(59));
        assertEquals("1m", SwapListenerImpl.elapsed(60));
        assertEquals("1m 30s", SwapListenerImpl.elapsed(90));
        assertEquals("10m", SwapListenerImpl.elapsed(600));
    }

    @Test
    public void theBuildPhaseHasItsOwnHeartbeatText() {
        assertTrue(SwapListenerImpl.sweepBuildLog(40).contains("40s"));
        assertTrue(SwapListenerImpl.sweepBuildStatus(40).contains("broadcasting"));
    }

    @Test
    public void theSweepHeartbeatIsAboutOneMoneroBlockApart() {
        assertEquals(120, SwapListenerImpl.SWEEP_HEARTBEAT_SECONDS);
        assertEquals(30, SwapListenerImpl.SYNC_HEARTBEAT_SECONDS);
    }

    @Test
    public void theBitcoinLockHeartbeatIsTenMinutesAndCountsInMinutes() {
        assertEquals(600, SwapListenerImpl.BTC_LOCK_HEARTBEAT_SECONDS);
        assertEquals("Still waiting for the Bitcoin lock transaction to be confirmed (10m)...", SwapListenerImpl.btcLockWaitLog(600));
        assertTrue(SwapListenerImpl.btcLockWaitLog(1800).contains("30m"));
        assertTrue(SwapListenerImpl.btcLockWaitStatus(600).contains("10m"));
    }
}
