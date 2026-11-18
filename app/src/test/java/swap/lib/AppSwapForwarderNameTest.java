package swap.lib;

import static org.junit.Assert.assertEquals;

import org.junit.Test;

/** The daemon-pool forwarder must be found and runnable on Windows, like asb.exe and price_ticker.exe. */
public class AppSwapForwarderNameTest {
    @Test
    public void windowsUsesExeNames() {
        assertEquals("/binaries/daemon_pool_proxy.exe", AppSwap.forwarderResource(true));
        assertEquals("daemon_pool_proxy.exe", AppSwap.forwarderFileName("daemon_pool_proxy", true));
        assertEquals("daemon_pool_proxy_electrum.exe", AppSwap.forwarderFileName("daemon_pool_proxy_electrum", true));
    }

    @Test
    public void macAndLinuxKeepPlainNames() {
        assertEquals("/binaries/daemon_pool_proxy", AppSwap.forwarderResource(false));
        assertEquals("daemon_pool_proxy", AppSwap.forwarderFileName("daemon_pool_proxy", false));
    }
}
