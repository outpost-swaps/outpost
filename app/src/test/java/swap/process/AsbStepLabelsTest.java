package swap.process;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public class AsbStepLabelsTest {
    @Test
    public void engineStatesReadInPlainWords() {
        assertEquals("Setup", AsbStepLabels.forState("started"));
        assertEquals("XMR confirming", AsbStepLabels.forState("xmr lock transfer proof sent"));
        assertEquals("Buyer signed", AsbStepLabels.forState("encrypted signature is learned"));
        assertEquals("Redeemed", AsbStepLabels.forState("btc is redeemed"));
    }

    @Test
    public void unknownAndEmptyStatesAreSafe() {
        assertEquals("Running", AsbStepLabels.forState(null));
        assertEquals("Running", AsbStepLabels.forState(" "));
        assertEquals("some new state", AsbStepLabels.forState("some new state"));
    }
}
