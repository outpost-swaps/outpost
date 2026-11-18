package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import java.util.List;
import java.util.Random;
import org.junit.Test;

public class HelperServerChoiceTest {
    @Test
    public void aTypedElectrumServerIsStoredWithItsSchemeAndPort() {
        assertEquals("tcp://abc.onion:50001", HelperServerChoice.normalizeElectrum(" abc.onion:50001 "));
        assertEquals("ssl://electrum.example.org:50002", HelperServerChoice.normalizeElectrum("SSL://electrum.example.org:50002"));
        assertEquals("tcp://abc.onion:50001", HelperServerChoice.normalizeElectrum("tcp://abc.onion:50001/"));
    }

    @Test
    public void aWrongElectrumServerGetsAMessageForTheUser() {
        assertTrue(assertThrows(IllegalArgumentException.class, () -> HelperServerChoice.normalizeElectrum("")).getMessage().startsWith("Enter the Electrum server"));
        assertTrue(assertThrows(IllegalArgumentException.class, () -> HelperServerChoice.normalizeElectrum("abc.onion")).getMessage().startsWith("Add the port"));
        assertTrue(assertThrows(IllegalArgumentException.class, () -> HelperServerChoice.normalizeElectrum("http://abc.onion:50001")).getMessage().contains("tcp:// or ssl://"));
        assertTrue(assertThrows(IllegalArgumentException.class, () -> HelperServerChoice.normalizeElectrum("tcp://abc.onion:50001/path")).getMessage().contains("only"));
    }

    @Test
    public void aTypedMoneroNodeIsStoredAsHttpWithItsPort() {
        assertEquals("http://node.onion:18089", HelperServerChoice.normalizeMonero("node.onion:18089"));
        assertEquals("https://node.example.org:18089", HelperServerChoice.normalizeMonero("https://node.example.org:18089"));
        assertTrue(assertThrows(IllegalArgumentException.class, () -> HelperServerChoice.normalizeMonero("node.onion")).getMessage().startsWith("Add the port"));
        assertTrue(assertThrows(IllegalArgumentException.class, () -> HelperServerChoice.normalizeMonero("tcp://node.onion:18089")).getMessage().contains("http://"));
    }

    @Test
    public void theDefaultElectrumServerAvoidsSslLikeTheFirstStartPick() {
        List<String> curated = List.of("ssl://a.example:50002", "tcp://b.onion:50001", "tcp://c.onion:50001");
        for (int seed = 0; seed < 20; seed++) {
            assertTrue(HelperServerChoice.defaultElectrum(curated, new Random(seed)).startsWith("tcp://"));
        }
        assertEquals("ssl://a.example:50002", HelperServerChoice.defaultElectrum(List.of("ssl://a.example:50002"), new Random(1)));
        assertNull(HelperServerChoice.defaultElectrum(List.of(), new Random(1)));
    }
}
