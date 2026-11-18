package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import java.util.List;
import org.junit.Test;

public class HelperOwnNodesTest {
    private static final String MINE = "http://192.168.1.5:18081";
    private static final String PUBLIC_A = "http://aaaa.onion:18081";
    private static final String PUBLIC_B = "https://node.example.org:18089";

    @Test
    public void localAddressesAreLocalAndPublicOnesAreNot() {
        assertTrue(HelperOwnNodes.isLocal("http://127.0.0.1:18081"));
        assertTrue(HelperOwnNodes.isLocal("http://localhost:18081"));
        assertTrue(HelperOwnNodes.isLocal("http://10.0.0.2:18081"));
        assertTrue(HelperOwnNodes.isLocal("http://172.20.1.1:18081"));
        assertTrue(HelperOwnNodes.isLocal("http://[::1]:18081"));
        assertFalse(HelperOwnNodes.isLocal("http://172.32.1.1:18081"));
        assertFalse(HelperOwnNodes.isLocal("http://8.8.8.8:18081"));
        assertFalse(HelperOwnNodes.isLocal(PUBLIC_A));
        assertFalse(HelperOwnNodes.isLocal(PUBLIC_B));
        assertFalse(HelperOwnNodes.isLocal(null));
    }

    @Test
    public void addNodeSuggestsOwnForLocalAddressesOnlyNeverForAnOnion() {
        assertTrue(HelperOwnNodes.suggestOwn("192.168.1.5"));
        assertTrue(HelperOwnNodes.suggestOwn("http://127.0.0.1:18081"));
        // most public Monero nodes are onions; an own Monero node is trusted alone for the lock check
        assertFalse(HelperOwnNodes.suggestOwn(PUBLIC_A));
        assertFalse(HelperOwnNodes.suggestOwn(PUBLIC_B));
    }

    @Test
    public void ownIsMarkedOrLocalButAnOnionIsNotOwnByItself() {
        String saved = HelperOwnNodes.with("[]", PUBLIC_B, true);
        assertTrue(HelperOwnNodes.isOwn(saved, PUBLIC_B));
        assertTrue(HelperOwnNodes.isOwn("[]", "http://127.0.0.1:18081"));
        assertFalse(HelperOwnNodes.isOwn("[]", PUBLIC_A));
        assertFalse(HelperOwnNodes.isOwn(null, PUBLIC_A));
    }

    @Test
    public void markingAndUnmarkingRoundTrips() {
        String saved = HelperOwnNodes.with(null, PUBLIC_A, true);
        saved = HelperOwnNodes.with(saved, PUBLIC_A, true);
        assertEquals("[\"http://aaaa.onion:18081\"]", saved);
        saved = HelperOwnNodes.with(saved, PUBLIC_A, false);
        assertEquals("[]", saved);
    }

    @Test
    public void aDamagedSavedValueMeansNoneMarked() {
        assertFalse(HelperOwnNodes.contains("not json", PUBLIC_A));
    }

    @Test
    public void theRadiosSelectTheRememberedNodeOfThatKind() {
        String saved = HelperOwnNodes.with("[]", MINE, true);
        List<String> shown = List.of(PUBLIC_A, MINE, PUBLIC_B);
        assertEquals(MINE, HelperOwnNodes.pick(true, MINE, PUBLIC_A, saved, shown));
        assertEquals(PUBLIC_B, HelperOwnNodes.pick(false, MINE, PUBLIC_B, saved, shown));
    }

    @Test
    public void withoutAMemoryTheFirstNodeOfThatKindIsUsed() {
        String saved = HelperOwnNodes.with("[]", MINE, true);
        List<String> shown = List.of(PUBLIC_A, MINE, PUBLIC_B);
        assertEquals(MINE, HelperOwnNodes.pick(true, null, null, saved, shown));
        assertEquals(PUBLIC_A, HelperOwnNodes.pick(false, null, null, saved, shown));
    }

    @Test
    public void aRememberedNodeOfTheWrongKindOrNotShownIsIgnored() {
        String saved = HelperOwnNodes.with("[]", MINE, true);
        List<String> shown = List.of(PUBLIC_A, MINE);
        assertEquals(PUBLIC_A, HelperOwnNodes.pick(false, null, MINE, saved, shown));
        assertEquals(MINE, HelperOwnNodes.pick(true, "http://gone:1", null, saved, shown));
    }

    @Test
    public void noOwnNodeSavedMeansThereIsNothingToSelect() {
        assertNull(HelperOwnNodes.pick(true, null, null, "[]", List.of(PUBLIC_A, PUBLIC_B)));
    }
}
