package swap.helper;

import org.bitcoinj.core.NetworkParameters;
import org.bitcoinj.params.MainNetParams;
import org.bitcoinj.params.TestNet3Params;

/**
 * Maps bitcoinj network parameters to the {@code testnet} flag the Rust side takes.
 *
 * <p>The requests used {@code params == TestNet3Params.get()}, which turns every network that is
 * not testnet3 (signet, regtest, a future one) into {@code testnet=false}, i.e. mainnet. Only the
 * two supported networks are accepted here; anything else is an error.
 */
public final class HelperNetwork {

    private HelperNetwork() {}

    public static boolean isTestnet(NetworkParameters params) {
        if (params == null) {
            throw new IllegalArgumentException("No Bitcoin network configured");
        }
        if (params.equals(MainNetParams.get())) {
            return false;
        }
        if (params.equals(TestNet3Params.get())) {
            return true;
        }
        throw new IllegalArgumentException("Unsupported Bitcoin network: " + params.getId());
    }
}
