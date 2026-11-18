package swap.helper;

import java.util.Locale;
import java.util.Set;

/**
 * Endpoints that are known-dead (removed from the defaults after repeated failed
 * probe passes — see notes/endpoint-inventory.md). A saved pick of any of these in
 * atomic-swaps.properties must be cleared on load so the defaults re-pick runs;
 * otherwise the dead endpoint overrides fixed defaults forever (the 2026-10-11
 * audit-miss: the saved Samourai relic electrum onion kept the app "connected" to
 * a dead endpoint).
 */
public final class HelperDeadEndpoints {

    /** Full-URL denies: same host as a live default but a dead/wrong-network scheme/port combo. */
    private static final Set<String> DEAD_URLS = Set.of(
            "ssl://blockstream.info:50002",
            // Testnet3 electrum servers (genesis-verified 2026-10-11, WORKLOG §39) — the
            // app is testnet4; exact-URL denies because the hosts also serve testnet4
            // on other ports (e.g. testnet.aranguren.org:52001, blackie.c3-soft.com:57010)
            "ssl://electrum.blockstream.info:60002",
            "ssl://blockstream.info:993",
            "tcp://testnet.aranguren.org:51001",
            "ssl://blackie.c3-soft.com:57006",
            "tcp://3tc6nefii2fwoc66dqvrwcyj64dd3r35ihgxvp4u37itsopns5fjtead.onion:50001",
            "tcp://gsw6sn27quwf6u3swgra6o7lrp5qau6kt3ymuyoxgkth6wntzm2bjwyd.onion:51001"
    );

    /** Host-fragment denies: every endpoint on the host is dead (any scheme/port). */
    private static final String[] DEAD_HOST_FRAGMENTS = {
            // Samourai-era relics (seized April 2024)
            "explorerzydxu5",
            "446unwib",
            // Removed after the 2026-10-08 probe pass
            "testnet.qtornado.com",
            "kittyserver.ddnsfree.com",
            "electrum.emzy.de",
            "electrum.bitaroo.net",
            "wallet.blither.io",
            "node.supportxmr", // .ir typo variant and .com (dead 2026-10-08 + 10-11)
            // Removed after the 2026-10-11 re-probe (two consecutive dead passes or NXDOMAIN)
            "node.moneroworld.com",
            "stagenet.community.rino.io",
            "tiopyrxsecon", // mysu mainnet onion
            "hashvaultsvg2", // hashvault mainnet onion (clearnet nodes.hashvault.pro is alive)
            "ykqlrp7", // xmr-tw stagenet onion
            // Removed 2026-11-14: answers every request before server.version with "use server.version
            // to identify client" and the swap wallet's Electrum client never sends it first, so the
            // ASB's Bitcoin wallet cannot start on it. A saved pick must be cleared so the defaults re-pick.
            "bitcoin.mullvad.net"
    };

    private HelperDeadEndpoints() {
    }

    /** True if the given endpoint URL is on the known-dead list. Null/empty is never dead. */
    public static boolean isDead(String url) {
        if (url == null || url.isEmpty()) {
            return false;
        }
        String normalized = url.trim().toLowerCase(Locale.ROOT);
        if (DEAD_URLS.contains(normalized)) {
            return true;
        }
        String host = hostOf(normalized);
        if (host == null) {
            return false;
        }
        for (String fragment : DEAD_HOST_FRAGMENTS) {
            if (host.contains(fragment)) {
                return true;
            }
        }
        return false;
    }

    private static String hostOf(String url) {
        int schemeEnd = url.indexOf("://");
        if (schemeEnd < 0) {
            return url;
        }
        int hostStart = schemeEnd + 3;
        int portStart = url.lastIndexOf(':');
        return portStart > hostStart ? url.substring(hostStart, portStart) : url.substring(hostStart);
    }
}
