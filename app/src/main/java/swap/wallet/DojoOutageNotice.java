package swap.wallet;

/**
 * Decides when a Dojo outage is worth a log line. Over Tor a request fails now and then and the
 * next refresh (15 s later) succeeds; those blips only update the status indicator. The log
 * warns once an outage has lasted {@link #GRACE_MILLIS}, and says it recovered only if it warned.
 */
final class DojoOutageNotice {
    static final long GRACE_MILLIS = 60_000;

    private Long failingSince;
    private boolean announced;

    /** A request failed at {@code nowMillis}: true exactly once per outage, after the grace. */
    synchronized boolean failed(long nowMillis) {
        if (failingSince == null) failingSince = nowMillis;
        if (!announced && nowMillis - failingSince >= GRACE_MILLIS) {
            announced = true;
            return true;
        }
        return false;
    }

    /** A request succeeded: true if the outage had been announced (so the recovery is logged). */
    synchronized boolean succeeded() {
        failingSince = null;
        boolean wasAnnounced = announced;
        announced = false;
        return wasAnnounced;
    }
}
