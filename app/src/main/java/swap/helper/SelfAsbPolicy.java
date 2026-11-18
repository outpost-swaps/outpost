package swap.helper;

/** Release policy for selecting the locally managed ASB as a seller. */
public final class SelfAsbPolicy {
    public static final String ALLOW_ENV = "OUTPOST_ALLOW_SELF_ASB";

    private SelfAsbPolicy() {
    }

    public static boolean isAllowedByEnvironment() {
        return isAllowedValue(System.getenv(ALLOW_ENV));
    }

    static boolean isAllowedValue(String value) {
        return "1".equals(value);
    }

    public static boolean isOwnAsb(String multiaddr, String peerId) {
        return isOwnAsb(multiaddr, peerId, null);
    }

    public static boolean isOwnAsb(String multiaddr, String peerId, String externalAddress) {
        if (multiaddr == null) return false;
        if (peerId != null && !peerId.isBlank() && multiaddr.contains(peerId)) return true;
        if (externalAddress == null || externalAddress.isBlank()) return false;
        String own = externalAddress.trim();
        return multiaddr.equals(own) || multiaddr.startsWith(own + "/");
    }

    public static String resolveSelfAsbMultiaddr(String multiaddr, String peerId,
                                                 String externalAddress, boolean allowed) {
        if (allowed && isOwnAsb(multiaddr, peerId, externalAddress) && peerId != null && !peerId.isBlank()) {
            // Dial the ASB the way any buyer would: through its published onion.
            // The loopback rewrite failed to dial on the v5 swarm (2026-11-13);
            // the onion works through our own Tor and keeps one code path.
            String own = externalAddress == null ? "" : externalAddress.trim();
            if (own.startsWith("/onion3/") || own.startsWith("/onion/")) {
                return own + "/p2p/" + peerId;
            }
            return "/ip4/127.0.0.1/tcp/" + swap.lib.AppAsb.ASB_PORT + "/p2p/" + peerId;
        }
        return multiaddr;
    }

    public static boolean shouldReject(String multiaddr, boolean asbRunning, String peerId, boolean allowed) {
        return shouldReject(multiaddr, asbRunning, peerId, null, allowed);
    }

    public static boolean shouldReject(String multiaddr, boolean asbRunning, String peerId,
                                       String externalAddress, boolean allowed) {
        return asbRunning && !allowed && isOwnAsb(multiaddr, peerId, externalAddress);
    }
}
