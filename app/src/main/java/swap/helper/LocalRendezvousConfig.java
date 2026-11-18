package swap.helper;

import swap.model.Multiaddr;

import java.util.Optional;

/** Parses the explicit dev-only local rendezvous multiaddr. */
public final class LocalRendezvousConfig {
    public static final String ENV = "OUTPOST_LOCAL_RENDEZVOUS_MULTIADDR";

    private LocalRendezvousConfig() {
    }

    public static Optional<Multiaddr> fromEnvironment() {
        return parse(System.getenv(ENV));
    }

    static Optional<Multiaddr> parse(String value) {
        if (value == null || value.isBlank()) return Optional.empty();
        try {
            return Optional.of(HelperAddress.parseMultiaddrUnchecked(value.trim()));
        } catch (RuntimeException e) {
            return Optional.empty();
        }
    }
}
