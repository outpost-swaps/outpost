package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import org.json.JSONArray;
import org.json.JSONObject;
import org.junit.Test;

/** Uses the real signed entries of the bundled Dojo Bay snapshot: the signatures are the operators', not ours. */
public class HelperDojoSignedMessageTest {
    private static JSONArray nodes() throws Exception {
        try (InputStream in = HelperDojoSignedMessageTest.class.getResourceAsStream("/dojo-directory.json")) {
            return new JSONObject(new String(in.readAllBytes(), StandardCharsets.UTF_8)).getJSONArray("nodes");
        }
    }

    private static List<JSONObject> signedNodes() throws Exception {
        List<JSONObject> out = new ArrayList<>();
        JSONArray nodes = nodes();
        for (int i = 0; i < nodes.length(); i++) {
            JSONObject node = nodes.getJSONObject(i);
            if (node.has("signed") && node.has("paymentCode")) {
                out.add(node);
            }
        }
        return out;
    }

    @Test
    public void everyRealEntryWhoseEmbeddedAndAdvertisedPaymentCodesAgreeVerifiesAndYieldsItsOwnPairing() throws Exception {
        List<JSONObject> nodes = signedNodes();
        assertTrue("the snapshot should carry signed entries", nodes.size() >= 10);

        int verified = 0;
        int rejected = 0;
        for (JSONObject node : nodes) {
            // independent of the code under test: do the two payment codes in the data agree?
            java.util.regex.Matcher embedded = java.util.regex.Pattern.compile("PM8T[1-9A-HJ-NP-Za-km-z]+").matcher(node.getString("signed"));
            boolean agree = embedded.find() && embedded.group().equals(node.getString("paymentCode"));

            var signed = HelperDojoSignedMessage.verify(node.getString("signed"), node.getString("paymentCode"));

            assertEquals(node.getString("name") + (agree ? " should verify" : " must be rejected: it advertises another payment code"), agree, signed.isPresent());
            if (agree) {
                JSONObject pairing = node.getJSONObject("payload").getJSONObject("pairing");
                assertEquals(pairing.getString("url"), signed.get().url());
                assertEquals(pairing.getString("apikey"), signed.get().apiKey());
                verified++;
            } else {
                rejected++;
            }
        }
        assertTrue("most entries verify", verified >= 10);
        assertTrue("and the mismatching ones are caught", rejected >= 1);
    }

    @Test
    public void changingTheSignedTextBreaksTheSignature() throws Exception {
        JSONObject node = signedNodes().get(0);
        String signed = node.getString("signed");
        String url = node.getJSONObject("payload").getJSONObject("pairing").getString("url");

        String tampered = signed.replace(url, url.replace(".onion", "x.onion"));

        assertFalse(HelperDojoSignedMessage.verify(tampered, node.getString("paymentCode")).isPresent());
    }

    @Test
    public void aSwappedApiKeyIsRejectedToo() throws Exception {
        JSONObject node = signedNodes().get(0);
        String key = node.getJSONObject("payload").getJSONObject("pairing").getString("apikey");

        assertFalse(HelperDojoSignedMessage.verify(node.getString("signed").replace(key, "x".repeat(key.length())), node.getString("paymentCode")).isPresent());
    }

    @Test
    public void aValidBlockUnderAnotherOperatorsAdvertisedPaymentCodeIsRejected() throws Exception {
        List<JSONObject> nodes = signedNodes();
        JSONObject first = nodes.get(0);
        String other = null;
        for (JSONObject node : nodes) {
            if (!node.getString("paymentCode").equals(first.getString("paymentCode"))) {
                other = node.getString("paymentCode");
                break;
            }
        }
        assertTrue(other != null);

        assertFalse(HelperDojoSignedMessage.verify(first.getString("signed"), other).isPresent());
    }

    @Test
    public void garbageIsRejectedWithoutAnError() {
        assertFalse(HelperDojoSignedMessage.verify(null, null).isPresent());
        assertFalse(HelperDojoSignedMessage.verify("", "x").isPresent());
        assertFalse(HelperDojoSignedMessage.verify("hello", "PM8T").isPresent());
        assertFalse(HelperDojoSignedMessage.verify("-----BEGIN BITCOIN SIGNED MESSAGE-----\n{}\n-----BEGIN BITCOIN SIGNATURE-----\nabc\n-----END BITCOIN SIGNATURE-----", null).isPresent());
    }
}
