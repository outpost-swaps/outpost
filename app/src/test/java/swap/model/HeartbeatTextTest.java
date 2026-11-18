package swap.model;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/** Every heartbeat counts time the same way: seconds under a minute, then minutes. */
public class HeartbeatTextTest {
    @Test
    public void elapsedReadsInSecondsThenMinutes() {
        assertEquals("30s", HeartbeatText.elapsed(30));
        assertEquals("1m", HeartbeatText.elapsed(60));
        assertEquals("1m 30s", HeartbeatText.elapsed(90));
        assertEquals("2m", HeartbeatText.elapsed(120));
    }

    @Test
    public void ticksGiveExactMultiplesOfThePeriod() {
        assertEquals(90, HeartbeatText.atTick(3, 30));
        assertEquals(240, HeartbeatText.atTick(2, 120));
    }

    @Test
    public void thePreparingAndResumingLinesUseMinutes() {
        assertEquals("Still preparing swap... 30s elapsed (Bitcoin wallet scan / Dojo / Monero setup, all over Tor).",
                HeartbeatText.preparingSwap(30));
        assertTrue(HeartbeatText.preparingSwap(90).startsWith("Still preparing swap... 1m 30s elapsed"));
        assertTrue(HeartbeatText.preparingSwap(90).endsWith("slow Tor circuits can add a minute or two)."));
        assertTrue(HeartbeatText.resumingSwap(120).startsWith("Still resuming swap... 2m elapsed"));
    }
}
