package swap.gui.controller.pages;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public class AsbActivityStatsTest {

    // 2026-11-07: a swap waiting for its BTC redeem to confirm showed ACTIVE 0
    // because only "btc is locked" and "xmr is locked" were counted.
    @Test
    public void aSwapWaitingForItsRedeemToConfirmIsActive() {
        assertTrue(AsbActivityStats.isInFlightPastLock("bitcoin redeem transaction published"));
    }

    @Test
    public void everyStateBetweenXmrLockAndRedeemIsActive() {
        assertTrue(AsbActivityStats.isInFlightPastLock("xmr is ready to lock"));
        assertTrue(AsbActivityStats.isInFlightPastLock("xmr lock transaction constructed"));
        assertTrue(AsbActivityStats.isInFlightPastLock("xmr lock transaction sent"));
        assertTrue(AsbActivityStats.isInFlightPastLock("xmr lock transfer proof sent"));
        assertTrue(AsbActivityStats.isInFlightPastLock("encrypted signature is learned"));
    }

    @Test
    public void matchingIgnoresCaseAndSurroundingWhitespace() {
        assertTrue(AsbActivityStats.isInFlightPastLock("  Bitcoin Redeem Transaction Published "));
    }

    @Test
    public void terminalAndPreLockStatesAreNotCounted() {
        // Terminal states.
        assertFalse(AsbActivityStats.isInFlightPastLock("btc is redeemed"));
        assertFalse(AsbActivityStats.isInFlightPastLock("btc is punished"));
        assertFalse(AsbActivityStats.isInFlightPastLock("btc is refunded"));
        assertFalse(AsbActivityStats.isInFlightPastLock("xmr is refunded"));
        assertFalse(AsbActivityStats.isInFlightPastLock("safely aborted"));
        // Pre-lock states: old stuck rows must not inflate the count.
        assertFalse(AsbActivityStats.isInFlightPastLock("started"));
        assertFalse(AsbActivityStats.isInFlightPastLock("bitcoin lock transaction in mempool"));
        // The two states the strip already counts are handled elsewhere.
        assertFalse(AsbActivityStats.isInFlightPastLock("btc is locked"));
        assertFalse(AsbActivityStats.isInFlightPastLock("xmr is locked"));
        assertFalse(AsbActivityStats.isInFlightPastLock(null));
        assertFalse(AsbActivityStats.isInFlightPastLock(""));
    }

    @Test
    public void activeAddsAllInFlightCategories() {
        assertEquals(1, AsbActivityStats.active(0, 0, 1));
        assertEquals(4, AsbActivityStats.active(1, 2, 1));
        assertEquals(0, AsbActivityStats.active(0, 0, 0));
    }
}
