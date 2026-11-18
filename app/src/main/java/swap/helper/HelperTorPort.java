package swap.helper;

/** The SOCKS port of the app's own Tor, once it is up. */
public final class HelperTorPort {
    private HelperTorPort() {}

    /** Polls until Tor reports its proxy port; {@code null} if that takes longer than {@code maxMillis}. */
    public static Integer waitForSocksPort(long maxMillis) {
        long deadline = System.currentTimeMillis() + maxMillis;
        while (System.currentTimeMillis() < deadline) {
            try {
                org.berndpruenster.netlayer.tor.Tor tor = org.berndpruenster.netlayer.tor.Tor.getDefault();
                if (tor != null) {
                    int port = tor.getProxy("127.0.0.1").getPort();
                    if (port > 0) {
                        return port;
                    }
                }
            } catch (Throwable notYet) { // TorCtlException is not an Exception
                // Tor is still starting
            }
            try {
                Thread.sleep(2000);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                return null;
            }
        }
        return null;
    }
}
