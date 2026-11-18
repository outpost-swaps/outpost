package swap.lib;

import static org.junit.Assert.assertEquals;

import java.util.List;
import org.junit.Test;
import swap.model.ElectrumServer;

public class AppSwapElectrumMergeTest {
    private static ElectrumServer server(String name, String url) {
        return new ElectrumServer(name, url, ElectrumServer.Type.ONION, ElectrumServer.Network.MAINNET);
    }

    @Test
    public void curatedComesFirstThenTheUsersOwnThenDiscoveredInAFixedOrder() {
        List<ElectrumServer> merged = AppSwap.mergeElectrumServers(
                List.of(server("c1", "tcp://c1.onion:50001"), server("c2", "tcp://c2.onion:50001")),
                List.of(server("mine", "tcp://mine.onion:50001")),
                List.of(server("d1", "tcp://d1.onion:50001"), server("d2", "tcp://d2.onion:50001")));

        assertEquals(List.of("c1", "c2", "mine", "d1", "d2"), merged.stream().map(ElectrumServer::name).toList());
    }

    @Test
    public void aDiscoveredServerNeverReplacesACuratedOrCustomEntryWithTheSameUrl() {
        List<ElectrumServer> merged = AppSwap.mergeElectrumServers(
                List.of(server("curated", "tcp://same.onion:50001")),
                List.of(),
                List.of(server("Dojo Bay: found", "tcp://same.onion:50001"), server("new", "tcp://new.onion:50001")));

        assertEquals(List.of("curated", "new"), merged.stream().map(ElectrumServer::name).toList());
    }

    @Test
    public void theUsersOwnEntryReplacesACuratedOneWithTheSameUrl() {
        List<ElectrumServer> merged = AppSwap.mergeElectrumServers(
                List.of(server("curated", "tcp://same.onion:50001")), List.of(server("mine", "tcp://same.onion:50001")), List.of());

        assertEquals(List.of("mine"), merged.stream().map(ElectrumServer::name).toList());
    }
}
