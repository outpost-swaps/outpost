package swap.model;

/**
 * Whether the user pressed Disconnect, kept apart from the screens so it can be read anywhere (and
 * tested). It is set when Disconnect is confirmed and cleared when a new swap or resume begins. A
 * retry scheduled for a swap the user cancelled must not start another one (2026-11-14: Disconnect
 * at the deposit address was followed by an automatic restart of the swap).
 */
public final class UserDisconnect {
    private static volatile boolean disconnected;
    private static volatile boolean knownSafe;

    private UserDisconnect() {}

    /** The user pressed Disconnect; {@code bitcoinMayBeLocked} is what the Disconnect policy decided. */
    public static void note(boolean bitcoinMayBeLocked) {
        disconnected = true;
        knownSafe = !bitcoinMayBeLocked;
    }

    /** A new swap or resume begins: an earlier Disconnect no longer applies. */
    public static void clear() {
        disconnected = false;
        knownSafe = false;
    }

    public static boolean happened() {
        return disconnected;
    }

    /** True when the user disconnected before anything could be locked. */
    public static boolean knownSafe() {
        return knownSafe;
    }
}
