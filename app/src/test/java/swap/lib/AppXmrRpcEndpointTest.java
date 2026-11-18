package swap.lib;

import org.junit.Test;

import static org.junit.Assert.assertEquals;

/**
 * A wallet-RPC restart must keep the exact endpoint the swap engine was given.
 * If the port or credentials changed, every engine retry would fail with a
 * connection error for the rest of the swap.
 */
public class AppXmrRpcEndpointTest {

    @Test
    public void restartReusesTheExistingPortAndCredentials() {
        AppXmrRpc.RpcEndpoint endpoint =
                AppXmrRpc.parseEndpoint("http://user123:pass456@127.0.0.1:54321/json_rpc");

        assertEquals(54321, endpoint.port());
        assertEquals("user123", endpoint.user());
        assertEquals("pass456", endpoint.password());
    }

    @Test
    public void passwordMayContainColons() {
        AppXmrRpc.RpcEndpoint endpoint =
                AppXmrRpc.parseEndpoint("http://user:pa:ss@127.0.0.1:60001/json_rpc");

        assertEquals("user", endpoint.user());
        assertEquals("pa:ss", endpoint.password());
        assertEquals(60001, endpoint.port());
    }
}
