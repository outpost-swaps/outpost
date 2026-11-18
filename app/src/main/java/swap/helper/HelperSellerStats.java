package swap.helper;

import java.util.Collection;
import org.bitcoinj.core.Coin;
import swap.model.Seller;

/**
 * The "Max" and "Avg" under the Sellers table: how much BTC the listed sellers take in total and
 * their average price. Free of any window so it can be tested.
 */
public final class HelperSellerStats {
    private HelperSellerStats() {}

    /** Total of the sellers' maximums in BTC, and their average price in BTC per XMR (null when none has a price). */
    public record Stats(Coin totalMax, Coin averagePrice) {}

    public static Stats of(Collection<Seller> sellers) {
        long totalMaxSats = 0;
        long priceSumSats = 0;
        long priceCount = 0;
        for (Seller seller : sellers) {
            totalMaxSats += Coin.parseCoin(String.valueOf(seller.maxQuantity())).value;
            double price = seller.getPrice();
            if (price > 0) {
                priceSumSats += Math.round(price * 100_000_000);
                priceCount++;
            }
        }
        return new Stats(Coin.valueOf(totalMaxSats), priceCount > 0 ? Coin.valueOf(priceSumSats / priceCount) : null);
    }
}
