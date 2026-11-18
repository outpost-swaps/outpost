package swap.helper;

import java.util.function.Consumer;

/**
 * A quiet check of the saved Electrum server and Monero node shortly after start, over Tor, so a dead
 * server shows up in the log at once and not as 30 seconds of timeouts later (2026-11-13: a saved
 * onion server was dead and every start paid two 15 s timeouts). It only warns and never changes
 * the choice. The warning points to Settings, where Test connection and the list live.
 */
public final class HelperSavedServerCheck {
    private static final int TIMEOUT_MILLIS = 60_000;

    private HelperSavedServerCheck() {}

    /** The warning for a saved server that failed its probe, or null when it passed. */
    public static String warning(String what, String url, boolean ok, String reason) {
        if (ok) {
            return null;
        }
        String why = reason == null || reason.isBlank() ? "no answer" : reason.trim();
        return "Your saved " + what + " " + url + " did not pass a check (" + why + "). Change it in Settings.";
    }

    /** Probes both servers on a daemon thread and hands each warning to {@code warn}. Never throws. */
    public static void startInBackground(boolean testnet, String electrumUrl, String moneroUrl, Consumer<String> warn) {
        Thread thread = new Thread(() -> {
            try {
                Integer port = HelperTorPort.waitForSocksPort(120_000);
                if (port == null) {
                    return;
                }
                HelperElectrumProbe.Connector connector = HelperElectrumProbe.throughTor(port);
                if (electrumUrl != null && !electrumUrl.isBlank()) {
                    HelperElectrumProbe.Result r = HelperElectrumProbe.probe(electrumUrl, testnet, connector, TIMEOUT_MILLIS);
                    emit(warn, warning("Electrum server", electrumUrl, r.ok(), r.reason()));
                }
                if (moneroUrl != null && !moneroUrl.isBlank()) {
                    HelperMoneroProbe.Result r = HelperMoneroProbe.probe(moneroUrl, testnet, connector, TIMEOUT_MILLIS);
                    emit(warn, warning("Monero node", moneroUrl, r.ok(), r.reason()));
                }
            } catch (Throwable ignored) {
                // a convenience check: never disturb the app
            }
        }, "saved-server-check");
        thread.setDaemon(true);
        thread.start();
    }

    private static void emit(Consumer<String> warn, String line) {
        if (line != null && warn != null) {
            warn.accept(line);
        }
    }
}
