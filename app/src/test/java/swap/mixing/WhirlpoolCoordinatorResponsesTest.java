package swap.mixing;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.samourai.wallet.util.JSONUtils;
import com.samourai.whirlpool.protocol.WhirlpoolProtocol;
import com.samourai.whirlpool.protocol.rest.PoolInfo;
import com.samourai.whirlpool.protocol.rest.PoolsResponse;
import com.samourai.whirlpool.protocol.rest.Tx0DataResponseV2;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HashSet;
import java.util.Set;
import org.junit.Test;

/**
 * The mainnet Whirlpool coordinator's real replies, captured over Tor on 2026-11-14 (public data, nothing
 * spent), read by the same mapper the client uses. Auto Tx0 itself has never run live (it needs about
 * 0.0275 BTC), so this is the nearest check available: it pins what the client is given by the live
 * coordinator and what happened before the partner id was fixed.
 */
public class WhirlpoolCoordinatorResponsesTest {

    private static String fixture(String name) throws IOException {
        try (InputStream in = WhirlpoolCoordinatorResponsesTest.class.getResourceAsStream("/whirlpool/" + name)) {
            assertNotNull("missing fixture " + name, in);
            return new String(in.readAllBytes(), StandardCharsets.UTF_8);
        }
    }

    private static ObjectMapper mapper() {
        return JSONUtils.getInstance().getObjectMapper();
    }

    @Test
    public void thePoolsAreReadAndTheSettingsOptionsExist() throws Exception {
        PoolsResponse pools = mapper().readValue(fixture("coordinator-pools.json"), PoolsResponse.class);

        Set<String> ids = new HashSet<>();
        for (PoolInfo pool : pools.pools) {
            ids.add(pool.poolId);
        }
        // the two Pool Size choices in Settings must be pools the coordinator really runs
        assertTrue(ids.contains("0.025btc"));
        assertTrue(ids.contains("0.25btc"));
        for (PoolInfo pool : pools.pools) {
            if (pool.poolId.equals("0.025btc")) {
                assertEquals(2_500_000L, pool.denomination);
                assertEquals(125_000L, pool.feeValue); // 5% of the pool
            }
        }
    }

    @Test
    public void theTx0DataForAnAcceptedPartnerIsComplete() throws Exception {
        Tx0DataResponseV2 response = mapper().readValue(fixture("coordinator-tx0-accepted.json"), Tx0DataResponseV2.class);

        assertEquals(2, response.tx0Datas.length);
        for (Tx0DataResponseV2.Tx0Data data : response.tx0Datas) {
            assertFalse(data.feePayload64 == null || data.feePayload64.isEmpty());
            assertTrue(data.feeAddress.startsWith("bc1"));
            assertTrue(data.feeValue > 0);
        }
    }

    @Test
    public void aRejectedPartnerLeavesTheTx0ListNull() throws Exception {
        // "Partner not found: partnerId=outpostswaps" is read leniently into an object with no list, and
        // Tx0Service.fetchTx0Data then loops over null: "Cannot read the array length because array is null",
        // the message Whirlpool mixing showed on 2026-11-14.
        Tx0DataResponseV2 response = mapper().readValue(
                fixture("coordinator-tx0-partner-not-found.json"), Tx0DataResponseV2.class);

        assertNull(response.tx0Datas);
    }

    @Test
    public void mixingSendsThePartnerIdTheCoordinatorKnows() throws Exception {
        assertEquals("ashigaruterminal", WhirlpoolProtocol.PARTNER_ID_ASHIGARU);

        String source = Files.readString(Path.of("src/main/java/swap/mixing/MixingService.java"));
        assertTrue(source.contains("config.setPartner(com.samourai.whirlpool.protocol.WhirlpoolProtocol.PARTNER_ID_ASHIGARU)"));
        assertFalse(source.contains("setPartner(\"outpostswaps\")"));
        assertFalse("no hard-coded string partner id", source.contains("setPartner(\""));
    }
}
