package swap.helper;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import org.bitcoinj.params.MainNetParams;
import org.bitcoinj.params.RegTestParams;
import org.bitcoinj.params.TestNet3Params;
import org.junit.Test;

public class HelperNetworkTest {

    @Test
    public void mainnetIsNotTestnet() {
        assertFalse(HelperNetwork.isTestnet(MainNetParams.get()));
    }

    @Test
    public void testnet3IsTestnet() {
        assertTrue(HelperNetwork.isTestnet(TestNet3Params.get()));
    }

    @Test
    public void anyOtherNetworkIsRefusedInsteadOfBecomingMainnet() {
        // The old `params == TestNet3Params.get()` check sent testnet=false (mainnet) for these.
        assertThrows(IllegalArgumentException.class, () -> HelperNetwork.isTestnet(RegTestParams.get()));
    }

    @Test
    public void aMissingNetworkIsRefused() {
        assertThrows(IllegalArgumentException.class, () -> HelperNetwork.isTestnet(null));
    }
}
