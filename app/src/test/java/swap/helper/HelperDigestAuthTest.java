package swap.helper;

import org.junit.Test;

import java.util.Map;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

public class HelperDigestAuthTest {

    // The worked example from RFC 2617 section 3.5.
    private static final String CHALLENGE =
            "Digest realm=\"testrealm@host.com\", qop=\"auth,auth-int\", "
                    + "nonce=\"dcd98b7102dd2f0e8b11d0f600bfb0c093\", opaque=\"5ccc069c403ebaf9f0171e9517f40e41\"";

    @Test
    public void parsesTheChallengeParameters() {
        Map<String, String> params = HelperDigestAuth.parseChallenge(CHALLENGE);
        assertEquals("testrealm@host.com", params.get("realm"));
        assertEquals("auth,auth-int", params.get("qop"));
        assertEquals("dcd98b7102dd2f0e8b11d0f600bfb0c093", params.get("nonce"));
        assertEquals("5ccc069c403ebaf9f0171e9517f40e41", params.get("opaque"));
    }

    @Test
    public void computesTheRfc2617ResponseDigest() {
        Map<String, String> params = HelperDigestAuth.parseChallenge(CHALLENGE);
        String header = HelperDigestAuth.authorization("Mufasa", "Circle Of Life", "GET",
                "/dir/index.html", params, "0a4f113b", 1);
        assertTrue(header, header.contains("response=\"6629fae49393a05397450978507c4ef1\""));
        assertTrue(header, header.contains("nc=00000001"));
        assertTrue(header, header.contains("cnonce=\"0a4f113b\""));
        assertTrue(header, header.contains("opaque=\"5ccc069c403ebaf9f0171e9517f40e41\""));
    }

    @Test
    public void aNonDigestChallengeYieldsNoParameters() {
        assertTrue(HelperDigestAuth.parseChallenge("Basic realm=\"x\"").isEmpty());
        assertTrue(HelperDigestAuth.parseChallenge(null).isEmpty());
    }
}
