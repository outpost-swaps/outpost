package swap.model;

/**
 * Shared wording for the "still working" heartbeats in the swap log and status line, so every
 * one of them counts time the same way: "40s" under a minute, "2m" on whole minutes, otherwise
 * "2m 30s". Heartbeats count ticks rather than reading the clock, so a 2-minute heartbeat reads
 * exactly 2m, 4m, 6m instead of drifting to "2m 1s".
 */
public final class HeartbeatText {

    private HeartbeatText() {
    }

    public static String elapsed(long seconds) {
        if (seconds < 60) return seconds + "s";
        long minutes = seconds / 60, rest = seconds % 60;
        return rest == 0 ? minutes + "m" : minutes + "m " + rest + "s";
    }

    /** Elapsed time at the n-th tick of a heartbeat with this period. */
    public static long atTick(long tick, long periodSeconds) {
        return tick * periodSeconds;
    }

    public static String preparingSwap(long seconds) {
        return "Still preparing swap... " + elapsed(seconds)
                + " elapsed (Bitcoin wallet scan / Dojo / Monero setup, all over Tor"
                + (seconds >= 60 ? "; slow Tor circuits can add a minute or two)." : ").");
    }

    public static String resumingSwap(long seconds) {
        return "Still resuming swap... " + elapsed(seconds)
                + " elapsed (Bitcoin wallet scan / Dojo / Monero setup / redeem sweep, all over Tor"
                + (seconds >= 60 ? "; slow Tor circuits can add a minute or two)." : ").");
    }
}
