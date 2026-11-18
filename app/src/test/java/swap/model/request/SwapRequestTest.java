package swap.model.request;

import org.bitcoinj.params.MainNetParams;
import org.bitcoinj.params.RegTestParams;
import org.bitcoinj.params.TestNet3Params;
import org.json.JSONObject;
import org.junit.Test;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertThrows;

public class SwapRequestTest {

    @Test
    public void serializesSwapsAddressIndices() {
        SwapRequest request = new SwapRequest(
                "swap-id",
                "seed",
                "xmr-address",
                "tcp://electrum.onion:50001",
                "peer",
                "proxy",
                TestNet3Params.get(),
                9050,
                "refund-address",
                2147483643L,
                46,
                6,
                "http://dojo.onion/test/v2",
                "dojo-key");

        JSONObject json = request.toJson();

        assertEquals(46, json.getInt("swapsReceiveIndex"));
        assertEquals(6, json.getInt("swapsChangeIndex"));
        assertEquals("http://dojo.onion/test/v2", json.getString("dojoUrl"));
        assertEquals("dojo-key", json.getString("dojoApiKey"));
    }

    // The taker wallet needs more than one Electrum server to fail over to
    // (swaps f7a162f7 and bf5f78fa lost their only server for minutes).
    @Test
    public void serializesTheElectrumPool() {
        SwapRequest request = new SwapRequest(
                "swap-id", "seed", "xmr-address", "tcp://a.onion:50001", "peer", "proxy", null,
                TestNet3Params.get(), 9050, "refund-address", 2147483643L, 46, 6,
                "http://dojo.onion/test/v2", "dojo-key", null,
                java.util.List.of("tcp://a.onion:50001", "tcp://b.onion:50001"));

        org.json.JSONArray pool = request.toJson().getJSONArray("electrumPoolUrls");

        assertEquals(2, pool.length());
        assertEquals("tcp://a.onion:50001", pool.getString(0));
        assertEquals("tcp://b.onion:50001", pool.getString(1));
    }

    @Test
    public void requestsWithoutAPoolSerializeAnEmptyArray() {
        SwapRequest request = new SwapRequest(
                "swap-id", "seed", "xmr-address", "tcp://a.onion:50001", "peer", "proxy",
                TestNet3Params.get(), 9050, "refund-address", 2147483643L, 46, 6,
                "http://dojo.onion/test/v2", "dojo-key");

        assertEquals(0, request.toJson().getJSONArray("electrumPoolUrls").length());
        assertEquals(java.util.List.of(), request.electrumPoolUrls());
    }

    @Test
    public void aNullPoolIsTreatedAsEmpty() {
        SwapRequest request = new SwapRequest(
                "swap-id", "seed", "xmr-address", "tcp://a.onion:50001", "peer", "proxy", null,
                TestNet3Params.get(), 9050, "refund-address", 2147483643L, 46, 6,
                null, null, null, null);

        assertEquals(java.util.List.of(), request.electrumPoolUrls());
    }

    @Test
    public void resumeSerializesTheElectrumPool() {
        ResumeRequest request = new ResumeRequest(
                "seed", "swap-id", "tcp://a.onion:50001", "proxy", null, TestNet3Params.get(), 9050,
                2147483643L, 46, 6, "http://dojo.onion/test/v2", "dojo-key", null,
                java.util.List.of("tcp://a.onion:50001", "tcp://b.onion:50001"));

        org.json.JSONArray pool = request.toJson().getJSONArray("electrumPoolUrls");

        assertEquals(2, pool.length());
        assertEquals("tcp://b.onion:50001", pool.getString(1));
    }

    @Test
    public void resumeSerializesSwapsAddressIndices() {
        ResumeRequest request = new ResumeRequest(
                "seed",
                "swap-id",
                "tcp://electrum.onion:50001",
                "proxy",
                TestNet3Params.get(),
                9050,
                2147483643L,
                46,
                6,
                "http://dojo.onion/test/v2",
                "dojo-key");

        JSONObject json = request.toJson();

        assertEquals(46, json.getInt("swapsReceiveIndex"));
        assertEquals(6, json.getInt("swapsChangeIndex"));
        assertEquals("http://dojo.onion/test/v2", json.getString("dojoUrl"));
        assertEquals("dojo-key", json.getString("dojoApiKey"));
    }

    private static SwapRequest requestFor(org.bitcoinj.core.NetworkParameters params) {
        return new SwapRequest(
                "swap-id", "seed", "xmr-address", "tcp://electrum.onion:50001", "peer", "proxy",
                params, 9050, "refund-address", 2147483643L, 46, 6,
                "http://dojo.onion/test/v2", "dojo-key");
    }

    @Test
    public void selectsTheNetworkExplicitly() {
        assertEquals(true, requestFor(TestNet3Params.get()).toJson().getBoolean("testnet"));
        assertEquals(false, requestFor(MainNetParams.get()).toJson().getBoolean("testnet"));
    }

    // Before, any network other than testnet3 was serialized as testnet=false, i.e. mainnet.
    @Test
    public void anUnsupportedNetworkIsNeverSentAsMainnet() {
        assertThrows(IllegalArgumentException.class, () -> requestFor(RegTestParams.get()).toJson());
    }

    @Test
    public void serializesTheMoneroWitnessNodesAndWhetherTheyAreRequired() {
        SwapRequest withWitnesses = new SwapRequest(
                "swap-id", "seed", "xmr-address", "tcp://electrum.onion:50001", "peer", "proxy",
                null, TestNet3Params.get(), 9050, "refund-address", 2147483643L, 46, 6,
                "http://dojo.onion/test/v2", "dojo-key", "http://127.0.0.1:1",
                java.util.List.of(),
                java.util.List.of("http://a.onion:18081", "https://b.example.org:18081"), true);

        JSONObject json = withWitnesses.toJson();

        assertEquals(2, json.getJSONArray("xmrWitnessUrls").length());
        assertEquals("http://a.onion:18081", json.getJSONArray("xmrWitnessUrls").getString(0));
        assertEquals(true, json.getBoolean("xmrWitnessRequired"));
    }

    @Test
    public void sendsHowManyWitnessesAreUsedOutOfTheCandidates() {
        SwapRequest request = new SwapRequest(
                "swap-id", "seed", "xmr-address", "tcp://electrum.onion:50001", "peer", "proxy",
                null, TestNet3Params.get(), 9050, "refund-address", 2147483643L, 46, 6,
                "http://dojo.onion/test/v2", "dojo-key", "http://127.0.0.1:1",
                java.util.List.of(),
                java.util.List.of("http://a.onion:18081", "http://b.onion:18081", "http://c.onion:18081"), true, 2);

        assertEquals(2, request.toJson().getInt("xmrWitnessCount"));
        assertEquals(3, request.toJson().getJSONArray("xmrWitnessUrls").length());
    }

    @Test
    public void theOlderShapeSendsNoCountSoEveryUrlIsUsedAsGiven() {
        SwapRequest request = new SwapRequest(
                "swap-id", "seed", "xmr-address", "tcp://electrum.onion:50001", "peer", "proxy",
                null, TestNet3Params.get(), 9050, "refund-address", 2147483643L, 46, 6,
                "http://dojo.onion/test/v2", "dojo-key", "http://127.0.0.1:1",
                java.util.List.of(),
                java.util.List.of("http://a.onion:18081"), true);

        assertEquals(0, request.toJson().getInt("xmrWitnessCount"));
    }

    // Older call sites build requests without witnesses: none configured and none required.
    @Test
    public void aRequestWithoutWitnessesRequiresNone() {
        JSONObject json = requestFor(TestNet3Params.get()).toJson();

        assertEquals(0, json.getJSONArray("xmrWitnessUrls").length());
        assertEquals(false, json.getBoolean("xmrWitnessRequired"));
    }

    @Test
    public void aResumeRequestCarriesTheSameFields() {
        ResumeRequest resume = new ResumeRequest(
                "seed", "swap-id", "tcp://electrum.onion:50001", "proxy", null, TestNet3Params.get(), 9050,
                2147483643L, 46, 6, "http://dojo.onion/test/v2", "dojo-key", "http://127.0.0.1:1",
                java.util.List.of(), java.util.List.of("http://a.onion:18081"), true);

        JSONObject json = resume.toJson();

        assertEquals("http://a.onion:18081", json.getJSONArray("xmrWitnessUrls").getString(0));
        assertEquals(true, json.getBoolean("xmrWitnessRequired"));
    }
}
