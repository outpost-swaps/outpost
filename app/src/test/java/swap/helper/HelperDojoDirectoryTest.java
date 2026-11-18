package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

public class HelperDojoDirectoryTest {
    private static final String SAMPLE = "{\"generated_at\":\"2026-10-15T17:30:50Z\",\"nodes\":["
            + "{\"id\":\"a\",\"network\":\"mainnet\",\"name\":\"Zed\",\"paynym\":\"+zed\",\"country\":\"SG\",\"status\":\"active\",\"url\":\"http://z.onion/v2\",\"apikey\":\"k1\"},"
            + "{\"id\":\"b\",\"network\":\"mainnet\",\"name\":\"alpha\",\"paynym\":\"\",\"status\":\"active\",\"url\":\"http://a.onion/v2\",\"apikey\":\"k2\"},"
            + "{\"id\":\"c\",\"network\":\"mainnet\",\"name\":\"gone\",\"status\":\"inactive\",\"url\":\"http://g.onion/v2\",\"apikey\":\"k3\"},"
            + "{\"id\":\"d\",\"network\":\"testnet\",\"name\":\"t\",\"status\":\"active\",\"url\":\"http://t.onion/test/v2\",\"apikey\":\"k4\"},"
            + "{\"id\":\"e\",\"network\":\"mainnet\",\"name\":\"nokey\",\"status\":\"active\",\"url\":\"http://n.onion/v2\",\"apikey\":\"\"},"
            + "{\"id\":\"f\",\"network\":\"mainnet\",\"name\":\"ftp\",\"status\":\"active\",\"url\":\"ftp://x\",\"apikey\":\"k\"}]}";

    @Test
    public void onlyActiveEntriesWithAUrlAndKeyAreListed() {
        HelperDojoDirectory.Directory directory = HelperDojoDirectory.parse(SAMPLE, false);

        assertEquals(3, directory.entries().size());
        assertFalse(directory.entries().stream().anyMatch(e -> e.name().equals("gone") || e.name().equals("nokey") || e.name().equals("ftp")));
    }

    @Test
    public void entriesAreFilteredByNetworkAndSortedByName() {
        HelperDojoDirectory.Directory directory = HelperDojoDirectory.parse(SAMPLE, false);

        assertEquals(java.util.List.of("alpha", "Zed"), directory.forNetwork(false).stream().map(HelperDojoDirectory.Entry::name).toList());
        assertEquals(java.util.List.of("t"), directory.forNetwork(true).stream().map(HelperDojoDirectory.Entry::name).toList());
    }

    @Test
    public void theLabelShowsOperatorAndCountryWhenKnown() {
        HelperDojoDirectory.Directory directory = HelperDojoDirectory.parse(SAMPLE, false);

        assertEquals("Zed (+zed) · SG", directory.forNetwork(false).get(1).label());
        assertEquals("alpha", directory.forNetwork(false).get(0).label());
    }

    @Test
    public void aBrokenFileGivesAnEmptyListNotAnError() {
        assertTrue(HelperDojoDirectory.parse("not json").entries().isEmpty());
        assertTrue(HelperDojoDirectory.parse("{}").entries().isEmpty());
        assertEquals("unknown date", HelperDojoDirectory.parse("{}").snapshotDate());
    }

    @Test
    public void theShippedCopyIsVerifiedLikeAnyOtherAndKeepsOnlyEntriesWhoseSignaturesCheckOut() {
        HelperDojoDirectory.Directory directory = HelperDojoDirectory.bundled();

        assertFalse(directory.forNetwork(false).isEmpty());
        assertFalse(directory.forNetwork(true).isEmpty());
        for (HelperDojoDirectory.Entry entry : directory.entries()) {
            assertTrue(entry.name() + " must be an onion address (the app is Tor-only)", entry.url().contains(".onion"));
            assertFalse(entry.apiKey().isBlank());
        }
        // snapshot of 2026-11-15: 12 of the 13 active entries verify; Barnabus does not
        // (Christ_is_King and btcwrestle went inactive since the 2026-11-12 snapshot)
        assertEquals(12, directory.entries().size());
        assertEquals(1, directory.skipped());
        assertEquals("2026-11-15", directory.snapshotDate());
    }

    @Test
    public void anEntryWithAnUnverifiableSignatureIsNeverListed() {
        String unsigned = "{\"generated_at\":\"2026-11-09T00:00:00Z\",\"nodes\":[{\"id\":\"x\",\"network\":\"mainnet\",\"name\":\"fake\",\"status\":\"active\","
                + "\"paymentCode\":\"PM8Tfake\",\"signed\":\"nope\",\"payload\":{\"pairing\":{\"url\":\"http://evil.onion/v2\",\"apikey\":\"k\"}}}]}";

        HelperDojoDirectory.Directory directory = HelperDojoDirectory.parse(unsigned);

        assertTrue(directory.entries().isEmpty());
        assertEquals(1, directory.skipped());
    }

    @Test
    public void theIndexerAddressIsCarriedWhenThereIsOne() {
        HelperDojoDirectory.Directory directory = HelperDojoDirectory.parse(
                "{\"nodes\":[{\"id\":\"a\",\"network\":\"mainnet\",\"name\":\"with\",\"status\":\"active\",\"indexer_url\":\"tcp://i.onion:50001\",\"url\":\"http://a.onion/v2\",\"apikey\":\"k\"},"
                        + "{\"id\":\"b\",\"network\":\"mainnet\",\"name\":\"fallback\",\"status\":\"active\",\"detected_indexer\":\"tcp://d.onion:50001\",\"url\":\"http://b.onion/v2\",\"apikey\":\"k\"},"
                        + "{\"id\":\"c\",\"network\":\"mainnet\",\"name\":\"none\",\"status\":\"active\",\"url\":\"http://c.onion/v2\",\"apikey\":\"k\"}]}", false);

        java.util.Map<String, String> indexers = new java.util.HashMap<>();
        directory.entries().forEach(e -> indexers.put(e.name(), e.indexerUrl()));

        assertEquals("tcp://i.onion:50001", indexers.get("with"));
        assertEquals("tcp://d.onion:50001", indexers.get("fallback"));
        assertEquals(null, indexers.get("none"));
    }
}
