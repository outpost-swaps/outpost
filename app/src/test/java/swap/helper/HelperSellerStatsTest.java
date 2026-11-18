package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;

import java.util.List;
import org.bitcoinj.core.Coin;
import org.junit.Test;
import swap.model.Seller;

public class HelperSellerStatsTest {
    @Test
    public void totalsTheMaximumsAndAveragesThePrices() {
        List<Seller> sellers = List.of(
                new Seller("/dns4/a/p2p/1", 0.00001, 0.0002, 0.0778083, "100%"),
                new Seller("/dns4/b/p2p/2", 0.008, 0.015, 0.077864, "90%"));
        HelperSellerStats.Stats stats = HelperSellerStats.of(sellers);
        assertEquals(Coin.parseCoin("0.0152"), stats.totalMax());
        assertEquals(Coin.valueOf(Math.round((0.0778083 * 1e8 + 0.077864 * 1e8)) / 2), stats.averagePrice());
    }

    @Test
    public void noSellersMeansZeroAndNoAverage() {
        HelperSellerStats.Stats stats = HelperSellerStats.of(List.of());
        assertEquals(Coin.ZERO, stats.totalMax());
        assertNull(stats.averagePrice());
    }

    @Test
    public void aSellerWithoutAPriceStillCountsTowardsTheMaximum() {
        HelperSellerStats.Stats stats = HelperSellerStats.of(List.of(new Seller("/dns4/a/p2p/1", 0.001, 0.01, 0.0, "100%")));
        assertEquals(Coin.parseCoin("0.01"), stats.totalMax());
        assertNull(stats.averagePrice());
    }
}
