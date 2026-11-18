package swap.mixing;

import com.samourai.whirlpool.client.tx0.Tx0Preview;
import com.samourai.whirlpool.client.tx0.Tx0Previews;
import com.samourai.whirlpool.client.whirlpool.beans.Pool;
import com.samourai.whirlpool.client.whirlpool.beans.Tx0Data;
import org.junit.Test;

import java.util.LinkedHashMap;
import java.util.Map;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;

public class MixingServiceTest {

    private static Tx0Preview buildPreview(String poolId, long denominationSats) throws Exception {
        Pool pool = new Pool();
        pool.setPoolId(poolId);
        pool.setDenomination(denominationSats);

        Tx0Data tx0Data = new Tx0Data(poolId, "feePaymentCode", 0L, 0L, 0, "", new byte[]{0}, "feeAddress");

        return new Tx0Preview(pool, tx0Data, 0L, 0L, 0L, 0, 0, denominationSats, 0L, 1);
    }

    private static Tx0Previews buildPreviews(Tx0Preview... previews) {
        Map<String, Tx0Preview> byPoolId = new LinkedHashMap<>();
        for (Tx0Preview preview : previews) {
            byPoolId.put(preview.getPool().getPoolId(), preview);
        }
        return new Tx0Previews(byPoolId);
    }

    @Test
    public void prefersPoolMatchingConfiguredDenomination() throws Exception {
        Tx0Preview small = buildPreview("0.001btc", 100_000L);
        Tx0Preview target = buildPreview("0.01btc", 1_000_000L);
        Tx0Preview large = buildPreview("0.05btc", 5_000_000L);
        Tx0Previews previews = buildPreviews(small, target, large);

        Tx0Preview selected = MixingService.selectPool(previews, "0.01btc");

        assertEquals("0.01btc", selected.getPool().getPoolId());
    }

    @Test
    public void fallsBackToLargestPoolWhenNoPreferenceSet() throws Exception {
        Tx0Preview small = buildPreview("0.001btc", 100_000L);
        Tx0Preview large = buildPreview("0.05btc", 5_000_000L);
        Tx0Previews previews = buildPreviews(small, large);

        Tx0Preview selected = MixingService.selectPool(previews, null);

        assertEquals("0.05btc", selected.getPool().getPoolId());
    }

    @Test
    public void fallsBackToLargestPoolWhenPreferenceDoesNotMatchAnyPool() throws Exception {
        Tx0Preview small = buildPreview("0.001btc", 100_000L);
        Tx0Preview large = buildPreview("0.05btc", 5_000_000L);
        Tx0Previews previews = buildPreviews(small, large);

        // 0.5btc isn't in the preview set (balance too small to qualify) - should not error,
        // should fall back to the largest eligible pool instead.
        Tx0Preview selected = MixingService.selectPool(previews, "0.5btc");

        assertEquals("0.05btc", selected.getPool().getPoolId());
    }

    @Test
    public void returnsNullWhenNoPoolsEligible() {
        Tx0Previews previews = buildPreviews();

        Tx0Preview selected = MixingService.selectPool(previews, null);

        assertNull(selected);
    }

    @Test
    public void malformedPoolSizePreferenceFallsBackToLargest() throws Exception {
        Tx0Preview small = buildPreview("0.001btc", 100_000L);
        Tx0Preview large = buildPreview("0.05btc", 5_000_000L);
        Tx0Previews previews = buildPreviews(small, large);

        Tx0Preview selected = MixingService.selectPool(previews, "not-a-number");

        assertEquals("0.05btc", selected.getPool().getPoolId());
    }
}
