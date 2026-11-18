package swap.helper;

import static org.junit.Assert.assertThrows;

import java.util.List;
import java.util.concurrent.atomic.AtomicBoolean;
import org.junit.Test;

public class HelperSwapGuardTest {

    @Test
    public void startingIsAllowedWhenNoSwapRuns() {
        HelperSwapGuard.requireNoRunningSwap(List.of());
        HelperSwapGuard.requireNoRunningSwap(List.of(new AtomicBoolean(false), new AtomicBoolean(false)));
    }

    // Two swaps would share the deposit wallet and could pick the same UTXO (first review #11);
    // before, only the UI stopped a second start.
    @Test
    public void aSecondSwapCannotStartWhileOneRuns() {
        assertThrows(
                IllegalStateException.class,
                () -> HelperSwapGuard.requireNoRunningSwap(
                        List.of(new AtomicBoolean(false), new AtomicBoolean(true))));
    }
}
