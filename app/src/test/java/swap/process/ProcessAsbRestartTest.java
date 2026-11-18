package swap.process;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public class ProcessAsbRestartTest {
    @Test
    public void aQueuedRestartIsSkippedWhenTheAsbInitializedAfterTheFailure() {
        assertFalse(ProcessAsb.shouldRestartAfterBackoff(1_000L, 2_000L));
    }

    @Test
    public void aQueuedRestartRunsWhenNoAsbInitializedSinceTheFailure() {
        assertTrue(ProcessAsb.shouldRestartAfterBackoff(1_000L, 0L));
        assertTrue(ProcessAsb.shouldRestartAfterBackoff(1_000L, 500L));
    }
}
