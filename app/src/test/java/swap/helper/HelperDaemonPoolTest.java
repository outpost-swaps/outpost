package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import java.util.List;
import org.junit.Test;

public class HelperDaemonPoolTest {
    private static final String ONION = "http://abcdefghijklmnop.onion:18089";
    private static final String HTTPS = "https://node.example.org:18089";
    private static final String PLAIN = "http://node.example.org:18081";
    private static final String SAVED = "http://saved.onion:18089";

    @Test
    public void onionAndHttpsDaemonsResistTamperingOnTheWay() {
        assertTrue(HelperDaemonPool.isTamperResistant(ONION));
        assertTrue(HelperDaemonPool.isTamperResistant(HTTPS));
        assertTrue(HelperDaemonPool.isTamperResistant("ssl://node.example.org:50002"));
        assertFalse(HelperDaemonPool.isTamperResistant(PLAIN));
        assertFalse(HelperDaemonPool.isTamperResistant("tcp://node.example.org:50001"));
        assertFalse(HelperDaemonPool.isTamperResistant(""));
        assertFalse(HelperDaemonPool.isTamperResistant(null));
        // a host that merely contains ".onion" is not an onion service
        assertFalse(HelperDaemonPool.isTamperResistant("http://x.onion.example.org:18081"));
    }

    // A plain-http clearnet daemon is reached through a Tor exit relay, which can rewrite its
    // answers, including the one that tells Bob Alice's Monero lock exists.
    @Test
    public void mainnetFailoverNeverAddsAPlainHttpClearnetDaemon() {
        List<String> pool = HelperDaemonPool.failoverCandidates(List.of(ONION, PLAIN, HTTPS, SAVED), SAVED, true);
        assertEquals(List.of(ONION, HTTPS), pool);
    }

    @Test
    public void testNetworksKeepTheWholeCuratedList() {
        List<String> pool = HelperDaemonPool.failoverCandidates(List.of(ONION, PLAIN, SAVED), SAVED, false);
        assertEquals(List.of(ONION, PLAIN), pool);
    }

    private static final String PICK_HTTP = "http://stagenet.xmr-tw.org:38081";
    private static final String PICK_HTTPS = "https://node.example.org:18089";
    private static final String PICK_ONION_A = "http://aaaa.onion:38089";
    private static final String PICK_ONION_B = "http://bbbb.onion:38089";

    @Test
    public void aFreshInstallNeverDrawsAPlainHttpClearnetNodeWhenAnOnionExists() {
        for (long seed = 0; seed < 200; seed++) {
            String picked = HelperDaemonPool.pickDefault(List.of(PICK_HTTP, PICK_HTTPS, PICK_ONION_A, PICK_ONION_B, "http://node2.monerodevs.org:38089"), new java.util.Random(seed));
            assertTrue(picked, picked.equals(PICK_ONION_A) || picked.equals(PICK_ONION_B));
        }
    }

    @Test
    public void withoutOnionsAnHttpsNodeIsPreferredToPlainHttp() {
        for (long seed = 0; seed < 50; seed++) {
            assertEquals(PICK_HTTPS, HelperDaemonPool.pickDefault(List.of(PICK_HTTP, PICK_HTTPS), new java.util.Random(seed)));
        }
    }

    @Test
    public void withNothingBetterTheListIsStillUsedAndAnEmptyListGivesNone() {
        assertEquals(PICK_HTTP, HelperDaemonPool.pickDefault(List.of(PICK_HTTP), new java.util.Random(1)));
        assertNull(HelperDaemonPool.pickDefault(List.of(), new java.util.Random(1)));
        assertNull(HelperDaemonPool.pickDefault(null, new java.util.Random(1)));
    }

    @Test
    public void bothOnionsAreReachableByTheRandomPick() {
        java.util.Set<String> seen = new java.util.HashSet<>();
        java.util.Random random = new java.util.Random(7); // one source: consecutive small seeds give correlated first values
        for (int i = 0; i < 100; i++) {
            seen.add(HelperDaemonPool.pickDefault(List.of(PICK_HTTP, PICK_ONION_A, PICK_ONION_B), random));
        }
        assertEquals(java.util.Set.of(PICK_ONION_A, PICK_ONION_B), seen);
    }
}
