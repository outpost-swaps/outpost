package swap.helper;

import org.junit.Test;
import swap.model.Multiaddr;

import java.util.Optional;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

public class LocalRendezvousConfigTest {
    private static final String PEER = "12D3KooWRWKn8xgiGk5fuHVa11yvG87748jYHxvTVhhLM5VEuw5s";
    private static final String LOCAL = "/ip4/127.0.0.1/tcp/8888/p2p/" + PEER;

    @Test
    public void parsesExplicitLocalMultiaddr() {
        Optional<Multiaddr> parsed = LocalRendezvousConfig.parse("  " + LOCAL + "  ");
        assertTrue(parsed.isPresent());
        assertEquals(LOCAL, parsed.get().toString());
    }

    @Test
    public void rejectsMissingOrMalformedMultiaddr() {
        assertFalse(LocalRendezvousConfig.parse(null).isPresent());
        assertFalse(LocalRendezvousConfig.parse("").isPresent());
        assertFalse(LocalRendezvousConfig.parse("127.0.0.1:8888").isPresent());
    }
}
