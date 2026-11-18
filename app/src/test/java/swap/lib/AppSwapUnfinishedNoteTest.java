package swap.lib;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;

import org.junit.Test;

public class AppSwapUnfinishedNoteTest {
    @Test
    public void oneOrNoneSaysNothing() {
        assertNull(AppSwap.otherUnfinishedNote(0));
        assertNull(AppSwap.otherUnfinishedNote(1));
    }

    @Test
    public void othersWaitInHistory() {
        assertEquals("1 other swap is unfinished. Resume it from History once this one finishes.", AppSwap.otherUnfinishedNote(2));
        assertEquals("2 other swaps are unfinished. Resume them from History once this one finishes.", AppSwap.otherUnfinishedNote(3));
    }
}
