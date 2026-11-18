package swap.lib;

import org.junit.Test;
import swap.helper.HelperProperties;

import java.util.Map;

import static org.junit.Assert.assertEquals;

public class AppAsbDojoEnvironmentTest {

    /** Audit M-6: the key is not in the ASB's environment; it goes over stdin with the seed. */
    @Test
    public void theKeyGoesOverStdinNotTheEnvironment() {
        String oldUrl = HelperProperties.dojoUrl;
        String oldKey = HelperProperties.dojoApiKey;
        try {
            HelperProperties.dojoUrl = "http://dojo.onion/test/v2";
            HelperProperties.dojoApiKey = "dojo-key";

            ProcessBuilder builder = new ProcessBuilder("true");
            builder.environment().put("OUTPOST_DOJO_API_KEY", "inherited-key");
            AppAsb.applyDojoEnvironment(builder);

            assertEquals("http://dojo.onion/test/v2", builder.environment().get("OUTPOST_DOJO_URL"));
            org.junit.Assert.assertNull(builder.environment().get("OUTPOST_DOJO_API_KEY"));
            assertEquals("c2VlZA==\ndojo-key",
                    new String(AppAsb.stdinPayload("c2VlZA==", "dojo-key"), java.nio.charset.StandardCharsets.UTF_8));
            assertEquals("c2VlZA==",
                    new String(AppAsb.stdinPayload("c2VlZA==", null), java.nio.charset.StandardCharsets.UTF_8));
            org.junit.Assert.assertThrows(IllegalStateException.class,
                    () -> AppAsb.stdinPayload("c2VlZA==", "a\nb"));
        } finally {
            HelperProperties.dojoUrl = oldUrl;
            HelperProperties.dojoApiKey = oldKey;
        }
    }

    @Test
    public void passesDecryptedDojoCredentialsToTheAsbProcess() {
        String oldUrl = HelperProperties.dojoUrl;
        String oldKey = HelperProperties.dojoApiKey;
        try {
            HelperProperties.dojoUrl = "http://dojo.onion/test/v2";
            HelperProperties.dojoApiKey = "dojo-key";

            ProcessBuilder builder = new ProcessBuilder("true");
            AppAsb.applyDojoEnvironment(builder);

            Map<String, String> env = builder.environment();
            assertEquals("http://dojo.onion/test/v2", env.get("OUTPOST_DOJO_URL"));
            // the key itself is in the stdin payload (theKeyGoesOverStdinNotTheEnvironment)
        } finally {
            HelperProperties.dojoUrl = oldUrl;
            HelperProperties.dojoApiKey = oldKey;
        }
    }

    @Test
    public void rejectsMissingOrPartialDojoCredentials() {
        String oldUrl = HelperProperties.dojoUrl;
        String oldKey = HelperProperties.dojoApiKey;
        try {
            HelperProperties.dojoUrl = "http://dojo.onion/test/v2";
            HelperProperties.dojoApiKey = null;

            ProcessBuilder builder = new ProcessBuilder("true");
            builder.environment().remove("OUTPOST_DOJO_URL");
            builder.environment().remove("OUTPOST_DOJO_API_KEY");

            try {
                AppAsb.applyDojoEnvironment(builder);
                throw new AssertionError("expected incomplete Dojo credentials to be rejected");
            } catch (IllegalStateException expected) {
                // expected: never let the ASB silently fall back to Electrum
            }

            HelperProperties.dojoUrl = null;
            HelperProperties.dojoApiKey = null;
            try {
                AppAsb.applyDojoEnvironment(builder);
                throw new AssertionError("expected missing Dojo credentials to be rejected");
            } catch (IllegalStateException expected) {
                // expected: the paired GUI ASB must not run without Dojo
            }
        } finally {
            HelperProperties.dojoUrl = oldUrl;
            HelperProperties.dojoApiKey = oldKey;
        }
    }
}
