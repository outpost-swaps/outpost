package swap.helper;

import org.junit.Test;
import swap.model.Multiaddr;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;

public class HelperAddressTest {

    private static final String PEER = "12D3KooWGRvf7qVQDrNR5nfYD6rKrbgeTi9x8RrbdxbmsPvxL4mw";

    @Test
    public void parseMultiaddr_roundTripsWssForm() {
        String wss = "/dns4/discovery.eigenwallet.org/tcp/443/wss/p2p/" + PEER;
        Multiaddr parsed = HelperAddress.parseMultiaddrUnchecked(wss);
        assertEquals(Multiaddr.Protocol.DNS4, parsed.addressProtocol());
        assertEquals("discovery.eigenwallet.org", parsed.address());
        assertEquals(Multiaddr.Protocol.TCP, parsed.netProtocol());
        assertEquals(443, parsed.port());
        assertEquals(Multiaddr.Protocol.WSS, parsed.webSocketProtocol());
        assertEquals(Multiaddr.Protocol.P2P, parsed.peerIdProtocol());
        assertEquals(PEER, parsed.peerId());
        assertEquals(wss, parsed.toString());
    }

    @Test
    public void parseMultiaddr_roundTripsPlainTcpForm() {
        String tcp = "/ip4/162.19.3.15/tcp/8888/p2p/" + PEER;
        Multiaddr parsed = HelperAddress.parseMultiaddrUnchecked(tcp);
        assertNull(parsed.webSocketProtocol());
        assertEquals(tcp, parsed.toString());
    }

    @Test
    public void parseMultiaddr_roundTripsOnionForm() {
        String onion = "/onion3/3xl2zfur4tpebogsrgn3l7l2illzkhwi3755jplmycmn4q77nxsrl6qd:8888/p2p/" + PEER;
        Multiaddr parsed = HelperAddress.parseMultiaddrUnchecked(onion);
        assertNull(parsed.webSocketProtocol());
        assertEquals("3xl2zfur4tpebogsrgn3l7l2illzkhwi3755jplmycmn4q77nxsrl6qd.onion", parsed.address());
        assertEquals(8888, parsed.port());
        assertEquals("/onion3/3xl2zfur4tpebogsrgn3l7l2illzkhwi3755jplmycmn4q77nxsrl6qd:8888/p2p/" + PEER,
                parsed.toString());
    }

    @Test
    public void parseMultiaddr_acceptsPastedOnionWithDotOnionSuffix() {
        String pasted = "/onion3/3xl2zfur4tpebogsrgn3l7l2illzkhwi3755jplmycmn4q77nxsrl6qd.onion:8888/p2p/" + PEER;
        Multiaddr parsed = HelperAddress.parseMultiaddrUnchecked(pasted);
        assertEquals("3xl2zfur4tpebogsrgn3l7l2illzkhwi3755jplmycmn4q77nxsrl6qd.onion", parsed.address());
        assertEquals(8888, parsed.port());
    }
}
