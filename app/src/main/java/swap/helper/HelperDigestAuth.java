package swap.helper;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.HashMap;
import java.util.Map;

/**
 * HTTP Digest authentication (RFC 2617, MD5, qop=auth) for the local
 * monero-wallet-rpc, which is started with {@code --rpc-login} and so answers an
 * unauthenticated request with 401. {@code java.net.http.HttpClient} sends no
 * credentials taken from a URL and does not implement Digest itself.
 */
public final class HelperDigestAuth {

    private HelperDigestAuth() {
    }

    /** Parses the parameters of a {@code WWW-Authenticate: Digest ...} header value. */
    public static Map<String, String> parseChallenge(String header) {
        Map<String, String> params = new HashMap<>();
        if (header == null) {
            return params;
        }
        String trimmed = header.trim();
        if (!trimmed.regionMatches(true, 0, "Digest", 0, 6)) {
            return params;
        }
        String rest = trimmed.substring(6).trim();
        int i = 0;
        while (i < rest.length()) {
            int eq = rest.indexOf('=', i);
            if (eq < 0) {
                break;
            }
            String key = rest.substring(i, eq).trim().toLowerCase();
            int valueStart = eq + 1;
            String value;
            int next;
            if (valueStart < rest.length() && rest.charAt(valueStart) == '"') {
                int close = rest.indexOf('"', valueStart + 1);
                if (close < 0) {
                    close = rest.length();
                }
                value = rest.substring(valueStart + 1, close);
                next = rest.indexOf(',', close);
            } else {
                int comma = rest.indexOf(',', valueStart);
                value = rest.substring(valueStart, comma < 0 ? rest.length() : comma).trim();
                next = comma;
            }
            params.put(key, value);
            if (next < 0) {
                break;
            }
            i = next + 1;
            while (i < rest.length() && rest.charAt(i) == ' ') {
                i++;
            }
        }
        return params;
    }

    /** Builds the {@code Authorization} header value answering {@code challenge}. */
    public static String authorization(String user, String password, String method, String uri,
                                       Map<String, String> challenge, String cnonce, int nonceCount) {
        String realm = challenge.getOrDefault("realm", "");
        String nonce = challenge.getOrDefault("nonce", "");
        String opaque = challenge.get("opaque");
        String qop = challenge.get("qop");
        boolean useQop = qop != null && qop.toLowerCase().contains("auth");

        String ha1 = md5(user + ":" + realm + ":" + password);
        String ha2 = md5(method + ":" + uri);
        String nc = String.format("%08x", nonceCount);
        String response = useQop
                ? md5(ha1 + ":" + nonce + ":" + nc + ":" + cnonce + ":auth:" + ha2)
                : md5(ha1 + ":" + nonce + ":" + ha2);

        StringBuilder header = new StringBuilder("Digest ")
                .append("username=\"").append(user).append("\", ")
                .append("realm=\"").append(realm).append("\", ")
                .append("nonce=\"").append(nonce).append("\", ")
                .append("uri=\"").append(uri).append("\", ")
                .append("algorithm=MD5, ")
                .append("response=\"").append(response).append("\"");
        if (useQop) {
            header.append(", qop=auth, nc=").append(nc).append(", cnonce=\"").append(cnonce).append("\"");
        }
        if (opaque != null) {
            header.append(", opaque=\"").append(opaque).append("\"");
        }
        return header.toString();
    }

    private static String md5(String value) {
        try {
            byte[] digest = MessageDigest.getInstance("MD5").digest(value.getBytes(StandardCharsets.UTF_8));
            StringBuilder hex = new StringBuilder();
            for (byte b : digest) {
                hex.append(String.format("%02x", b));
            }
            return hex.toString();
        } catch (NoSuchAlgorithmException e) {
            throw new IllegalStateException(e);
        }
    }
}
