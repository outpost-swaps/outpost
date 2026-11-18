package swap.gui.controller.popups;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

public class PopupSwapsInProgressTest {
    @Test
    public void subtitleCountsTheSwaps() {
        assertEquals("1 swap is running.", PopupSwapsInProgress.subtitle(1));
        assertEquals("3 swaps are running.", PopupSwapsInProgress.subtitle(3));
    }
}
