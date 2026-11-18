package swap.model;

import org.bitcoinj.core.Coin;
import org.bitcoinj.params.TestNet3Params;
import org.junit.Before;
import org.junit.Test;
import swap.helper.HelperProperties;
import swap.lib.AppSwap;
import swap.model.request.StartAsbRequest;

import java.lang.reflect.Field;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

public class StartAsbRequestManualPriceTest {

    @Before
    public void setUp() throws Exception {
        Path swapRoot = Files.createTempDirectory("swaps-asb-request-test");
        Field rootField = AppSwap.class.getDeclaredField("swapRootDir");
        rootField.setAccessible(true);
        rootField.set(null, swapRoot.toString());
        HelperProperties.network = "mainnet";
    }

    @Test
    public void manualPriceIsEmittedIntoTheMakerSection() {
        StartAsbRequest request = newRequest(Coin.parseCoin("0.007153"));

        String makerSection = String.join("\n", request.toConfigToml().get("maker"));

        assertTrue(makerSection.contains("manual_price = 0.007153"));
    }

    @Test
    public void noManualPriceMeansNoManualPriceLine() {
        StartAsbRequest request = newRequest(null);

        String makerSection = String.join("\n", request.toConfigToml().get("maker"));

        assertFalse(makerSection.contains("manual_price"));
    }

    private static StartAsbRequest newRequest(Coin manualPrice) {
        return new StartAsbRequest(
                "electrum.invalid:50001", List.of(), "127.0.0.1:9050", 9050, "http://127.0.0.1:18083",
                "http://127.0.0.1:18081", List.of(), false, 0.001f, 0.1f, 0.01f, manualPrice, TestNet3Params.get(), "dummy.onion");
    }
}
