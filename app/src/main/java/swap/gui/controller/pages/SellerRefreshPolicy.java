package swap.gui.controller.pages;

/**
 * Decides when the periodic seller-list refresh may run.
 *
 * Each refresh starts a libp2p swarm per rendezvous point and dials every
 * discovered seller over Tor: in swap f7a162f7 that was 137 Tor dial attempts
 * (54 of them onion services) every 2 minutes, while the funded swap's Electrum
 * connection shared the same Tor instance. The Electrum stalls of swaps
 * f7a162f7 and bf5f78fa fell on or right after those bursts. The correlation is
 * not proven, but the refresh has no value while a swap is running (the user
 * cannot start another), so the scheduled refresh is skipped then. The manual
 * Refresh button is not affected.
 */
final class SellerRefreshPolicy {

    /**
     * How often the seller list refreshes on its own. Was 2 minutes: each refresh dials every
     * seller once per rendezvous point through each of its addresses, which reached the same
     * Eigenwallet seller up to 13 times per refresh and got us rate-limited (HTTP 429) on
     * 2026-11-11 (WORKLOG §458). The Refresh button still refreshes on demand.
     */
    static final long SCHEDULED_REFRESH_MINUTES = 10;

    private SellerRefreshPolicy() {
    }

    static boolean shouldRunScheduledRefresh(boolean swapRunning) {
        return !swapRunning;
    }
}
