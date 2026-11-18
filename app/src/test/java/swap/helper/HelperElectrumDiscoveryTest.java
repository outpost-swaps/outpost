package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.io.IOException;
import java.net.Socket;
import java.util.List;
import org.junit.Test;
import swap.model.ElectrumServer;

public class HelperElectrumDiscoveryTest {
    private static HelperDojoDirectory.Entry entry(String name, boolean testnet, String indexer) {
        return new HelperDojoDirectory.Entry("id-" + name, name, "", "", testnet, "http://" + name + ".onion/v2", "k", indexer);
    }

    private static HelperElectrumProbe.Result result(String url, boolean ok, int height) {
        return new HelperElectrumProbe.Result(url, ok, 1000, height, ok ? "" : "x");
    }

    @Test
    public void onlyOnionTcpOrSslIndexersOfTheRightNetworkAreCandidatesOnePerHost() {
        HelperDojoDirectory.Directory directory = new HelperDojoDirectory.Directory("2026-11-09", List.of(
                entry("zeta", false, "tcp://zzz.onion:50001"),
                entry("alpha", false, "ssl://aaa.onion:50002"),
                entry("clearnet", false, "ssl://electrum.example.org:50002"),
                entry("plainhttp", false, "http://bbb.onion:80"),
                entry("zzz-duplicate", false, "tcp://zzz.onion:50001"),
                entry("noindexer", false, null),
                entry("noport", false, "tcp://ccc.onion"),
                entry("testnet", true, "tcp://ttt.onion:50001")));

        List<HelperElectrumDiscovery.Candidate> mainnet = HelperElectrumDiscovery.candidates(directory, false);

        assertEquals(List.of("tcp://zzz.onion:50001", "ssl://aaa.onion:50002").size(), mainnet.size());
        assertEquals("alpha", mainnet.get(0).name());
        assertEquals("zeta", mainnet.get(1).name());
        assertEquals(List.of("tcp://ttt.onion:50001"), HelperElectrumDiscovery.candidates(directory, true).stream().map(HelperElectrumDiscovery.Candidate::url).toList());
    }

    @Test
    public void aServerClaimingAFarHigherTipCannotMakeTheHonestOnesLookBehind() {
        List<HelperElectrumProbe.Result> results = List.of(
                result("a", true, 975189), result("b", true, 975190), result("c", true, 975189),
                result("liar", true, 999999), result("behind", true, 975100), result("down", false, 0));

        List<String> healthy = HelperElectrumDiscovery.healthy(results).stream().map(HelperElectrumProbe.Result::url).toList();

        assertEquals(List.of("a", "b", "c"), healthy);
    }

    @Test
    public void withFewerThanThreeAnswersThereIsNothingToCompareAndAllCount() {
        List<HelperElectrumProbe.Result> results = List.of(result("a", true, 5), result("b", true, 975189), result("down", false, 0));

        assertEquals(2, HelperElectrumDiscovery.healthy(results).size());
        assertTrue(HelperElectrumDiscovery.healthy(List.of(result("down", false, 0))).isEmpty());
    }

    @Test
    public void probingKeepsOnlyTheServersThatAnswerAndKeepsTheDirectoryOrder() {
        HelperElectrumProbe.Connector connector = (host, port, tls, timeout) -> { throw new IOException("unreachable"); };
        List<ElectrumServer> none = HelperElectrumDiscovery.probe(
                List.of(new HelperElectrumDiscovery.Candidate("tcp://aaa.onion:50001", "a")), false, connector, 500);

        assertTrue(none.isEmpty());
    }

    @Test
    public void discoveredServersAreForOneNetworkAtATime() {
        assertTrue(HelperElectrumDiscovery.discovered(false).isEmpty() || HelperElectrumDiscovery.discovered(true).isEmpty());
        assertFalse(HelperElectrumDiscovery.isDiscovered("tcp://never-listed.onion:50001"));
    }
}
