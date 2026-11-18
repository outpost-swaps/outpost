package swap.gui.controller.pages;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public class SwapStepLabelsTest {
    @Test
    public void finishedRowsSayHowTheyEnded() {
        assertEquals("sent", SwapStepLabels.completedDetail(1));
        assertEquals("confirmed", SwapStepLabels.completedDetail(2));
        assertEquals("confirmed", SwapStepLabels.completedDetail(3));
        assertEquals("claimed", SwapStepLabels.completedDetail(4));
        assertEquals("swept", SwapStepLabels.completedDetail(5));
    }

    @Test
    public void anUnknownRowStaysBlank() {
        assertEquals("", SwapStepLabels.completedDetail(0));
        assertEquals("", SwapStepLabels.completedDetail(6));
    }
}
