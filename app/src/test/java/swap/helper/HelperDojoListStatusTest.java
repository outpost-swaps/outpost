package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.util.List;
import org.junit.Test;

public class HelperDojoListStatusTest {
    private static HelperDojoDirectory.Directory dir(int skipped) {
        return new HelperDojoDirectory.Directory("2026-10-15T17:30:50Z", List.of(), skipped);
    }

    @Test
    public void aLiveListSaysHowManyVerifiedAndHowManyWereSkipped() {
        HelperDojoDirectory.Directory live = new HelperDojoDirectory.Directory("2026-11-09T12:00:00Z",
                List.of(new HelperDojoDirectory.Entry("a", "A", "", "", false, "http://a.onion", "k"),
                        new HelperDojoDirectory.Entry("b", "B", "", "", false, "http://b.onion", "k")), 3);

        String text = HelperDojoListStatus.text(HelperDojoDirectoryLoader.Source.LIVE, live, HelperDojoListStatus.Phase.DONE, null);

        assertEquals("Current list from Dojo Bay, loaded just now: 2 Dojos verified, 3 skipped because their signature did not check out.", text);
    }

    @Test
    public void aShippedListWarnsItMayBeOldAndSaysWhileItIsLoadingTheCurrentOne() {
        String text = HelperDojoListStatus.text(HelperDojoDirectoryLoader.Source.SHIPPED, dir(0), HelperDojoListStatus.Phase.LOADING, null);

        assertTrue(text.contains("shipped with the app, from 2026-10-15"));
        assertTrue(text.contains("may be out of date"));
        assertTrue(text.endsWith("Loading the current list over Tor..."));
    }

    @Test
    public void aFailureGivesTheReasonAndAlsoKeepsTheListInUse() {
        String text = HelperDojoListStatus.text(HelperDojoDirectoryLoader.Source.SAVED, dir(0), HelperDojoListStatus.Phase.FAILED, "timed out");

        assertTrue(text.startsWith("Last list loaded from Dojo Bay, 2026-10-15."));
        assertTrue(text.endsWith("Could not load the current list (timed out)."));
    }

    @Test
    public void aLiveListNeverCarriesALoadingOrFailureNote() {
        String text = HelperDojoListStatus.text(HelperDojoDirectoryLoader.Source.LIVE, dir(0), HelperDojoListStatus.Phase.LOADING, null);

        assertFalse(text.contains("Loading"));
    }
}
