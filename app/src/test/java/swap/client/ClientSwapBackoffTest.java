package swap.client;

import org.junit.Test;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

/**
 * The swap retry loop previously re-invoked the native entrypoint in a tight spin
 * on every error (re-initializing wallets and re-broadcasting). These tests pin the
 * exponential backoff schedule that replaced it.
 */
public class ClientSwapBackoffTest {

    @Test
    public void firstRetryWaitsTheInitialDelay() {
        assertEquals(5_000L, ClientSwap.nextBackoffMs(0));
    }

    @Test
    public void backoffDoublesEachConsecutiveRetry() {
        assertEquals(10_000L, ClientSwap.nextBackoffMs(5_000L));
        assertEquals(20_000L, ClientSwap.nextBackoffMs(10_000L));
        assertEquals(40_000L, ClientSwap.nextBackoffMs(20_000L));
    }

    @Test
    public void backoffIsCapped() {
        assertEquals(60_000L, ClientSwap.nextBackoffMs(40_000L));
        assertEquals(60_000L, ClientSwap.nextBackoffMs(60_000L));
        assertEquals(60_000L, ClientSwap.nextBackoffMs(120_000L));
    }

    @Test
    public void backoffNeverDecreasesOrGoesNegative() {
        long ms = 0;
        long previous = 0;
        for (int i = 0; i < 20; i++) {
            ms = ClientSwap.nextBackoffMs(ms);
            assertTrue(ms >= previous);
            previous = ms;
        }
        assertEquals(60_000L, ms);
    }
}
