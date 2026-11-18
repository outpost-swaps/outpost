package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.io.ByteArrayInputStream;
import java.nio.charset.StandardCharsets;
import org.junit.Test;

public class HelperMoneroProbeTest {
    private static HelperMoneroProbe.Result answer(String http, boolean stagenet) throws Exception {
        return HelperMoneroProbe.read("http://node.onion:38089", stagenet, new ByteArrayInputStream(http.getBytes(StandardCharsets.UTF_8)), System.currentTimeMillis());
    }

    private static String ok(String body) {
        return "HTTP/1.1 200 Ok\r\nContent-Type: application/json\r\n\r\n" + body;
    }

    @Test
    public void aSyncedNodeOnTheRightNetworkPasses() throws Exception {
        HelperMoneroProbe.Result result = answer(ok("{\"id\":\"0\",\"jsonrpc\":\"2.0\",\"result\":{\"status\":\"OK\",\"nettype\":\"stagenet\",\"height\":2249320,\"synchronized\":true}}"), true);
        assertTrue(result.ok());
        assertEquals(2249320, result.height());
    }

    @Test
    public void aNodeOnAnotherNetworkIsRejected() throws Exception {
        HelperMoneroProbe.Result result = answer(ok("{\"result\":{\"status\":\"OK\",\"nettype\":\"mainnet\",\"height\":3500000}}"), true);
        assertFalse(result.ok());
        assertTrue(result.reason().contains("mainnet"));
    }

    @Test
    public void aSyncingNodeOrAnErrorAnswerIsRejected() throws Exception {
        assertFalse(answer(ok("{\"result\":{\"status\":\"OK\",\"nettype\":\"mainnet\",\"synchronized\":false}}"), false).ok());
        assertFalse(answer(ok("{\"result\":{\"status\":\"BUSY\"}}"), false).ok());
        assertFalse(answer("HTTP/1.1 401 Unauthorized\r\n\r\n", false).ok());
        assertFalse(answer("SSH-2.0-OpenSSH\r\n", false).ok());
    }
}
