package swap.lib;

import org.junit.Test;

public class AppSwapElectrumEndpointTest {

    @Test(expected = IllegalStateException.class)
    public void rejectsDirectSslElectrumEndpoint() {
        AppSwap.rejectDirectElectrumSsl("ssl://blockstream.info:700");
    }

    @Test
    public void allowsOnionTcpElectrumEndpoint() {
        AppSwap.rejectDirectElectrumSsl(
                "tcp://k3tauelvqb724if4roauzo5v4u4ha74clouplx55o3xownsa5beigqad.onion:50001");
    }

    @Test
    public void allowsNullEndpoint() {
        AppSwap.rejectDirectElectrumSsl(null);
    }

    // Taker parity with legacy and with the ASB: connect straight to an onion
    // tcp:// server through Tor SOCKS. The local pool forwarder sits in the
    // wallet's startup path and its session hung on 2026-11-06 (swap 44c493a4)
    // while the upstream answered in under a second.
    private static final String ONION_A =
            "tcp://k3tauelvqb724if4roauzo5v4u4ha74clouplx55o3xownsa5beigqad.onion:50001";
    private static final String ONION_B =
            "tcp://xufljktdbo542e6jyc7hat6ybnxx6km57rhmw5qpbzyducdcofyx2kqd.onion:50001";

    @Test
    public void takerUsesConfiguredTcpServerDirectly() {
        org.junit.Assert.assertEquals(
                ONION_B, AppSwap.selectDirectElectrumEndpoint(ONION_B, java.util.List.of(ONION_A)));
    }

    @Test
    public void takerReplacesSslServerWithFirstDirectTcpPoolMember() {
        org.junit.Assert.assertEquals(
                ONION_A,
                AppSwap.selectDirectElectrumEndpoint(
                        "ssl://blockstream.info:700", java.util.List.of(ONION_A, ONION_B)));
    }

    @Test
    public void takerUsesFirstPoolMemberWhenNothingIsConfigured() {
        org.junit.Assert.assertEquals(
                ONION_A, AppSwap.selectDirectElectrumEndpoint(null, java.util.List.of(ONION_A)));
    }

    @Test
    public void takerFallsBackToForwarderWhenNoDirectTcpServerExists() {
        org.junit.Assert.assertNull(
                AppSwap.selectDirectElectrumEndpoint("ssl://blockstream.info:700", java.util.List.of()));
    }

    @Test
    public void withFailoverOffTheWalletGetsExactlyTheSavedServer() {
        java.util.List<String> pool = AppSwap.electrumPoolUrls(ONION_B, java.util.List.of(ONION_A, ONION_B), false);
        org.junit.Assert.assertEquals(java.util.List.of(ONION_B), pool);
    }

    @Test
    public void withFailoverOnTheSavedServerComesFirstThenTheCuratedPool() {
        java.util.List<String> pool = AppSwap.electrumPoolUrls(ONION_B, java.util.List.of(ONION_A, ONION_B), true);
        org.junit.Assert.assertEquals(java.util.List.of(ONION_B, ONION_A), pool);
    }

    @Test
    public void withFailoverOffNoSavedServerMeansNoPoolRatherThanACuratedOne() {
        org.junit.Assert.assertTrue(AppSwap.electrumPoolUrls(null, java.util.List.of(ONION_A), false).isEmpty());
    }
}
