package swap.process;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public class ProcessAsbResumedStateTest {
    @Test
    public void aNewSwapStartsAsSetup() {
        assertEquals("Setup", ProcessAsb.startedLabel("brand-new"));
    }

    @Test
    public void aResumedSwapKeepsItsSavedStateOnce() {
        ProcessAsb.noteResumedState("resumed-1", "xmr lock transfer proof sent");
        assertEquals("XMR confirming", ProcessAsb.startedLabel("resumed-1"));
        assertEquals("Setup", ProcessAsb.startedLabel("resumed-1"));
    }

    @Test
    public void aResumedSwapStillInSetupReadsSetup() {
        ProcessAsb.noteResumedState("resumed-2", "started");
        assertEquals("Setup", ProcessAsb.startedLabel("resumed-2"));
    }
}
