package swap.gui.controller.pages;

import java.util.Locale;

/**
 * Classifies persisted ASB swap states for the Liquidity page's activity strip.
 *
 * The strip counted "active" as swaps whose persisted state is exactly
 * "btc is locked" or "xmr is locked". A maker swap spends most of its life in
 * later states, and the longest wait of all is "bitcoin redeem transaction
 * published" (the redeem is broadcast and waiting for confirmations), so it
 * showed ACTIVE 0 while a swap was still waiting to be paid (2026-11-07).
 */
final class AsbActivityStats {

    private AsbActivityStats() {
    }

    /**
     * Persisted states after the BTC lock that are neither "btc is locked" nor
     * "xmr is locked" but are still in flight: the maker has funds committed
     * and the swap is not finished. Pre-lock states and terminal states are
     * deliberately excluded (old stuck rows must not inflate the count).
     */
    static boolean isInFlightPastLock(String status) {
        if (status == null) {
            return false;
        }
        return switch (status.trim().toLowerCase(Locale.ROOT)) {
            case "xmr is ready to lock",
                 "xmr lock transaction constructed",
                 "xmr lock transaction sent",
                 "xmr lock transfer proof sent",
                 "encrypted signature is learned",
                 "bitcoin redeem transaction published" -> true;
            default -> false;
        };
    }

    static long active(long btcLocked, long xmrLocked, long inFlightPastLock) {
        return btcLocked + xmrLocked + inFlightPastLock;
    }
}
