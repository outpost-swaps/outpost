package swap.helper;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.InetSocketAddress;
import java.net.Proxy;
import java.net.URI;
import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;
import java.util.Locale;

/**
 * Checks that a Dojo answers and accepts an API key, by logging in the way a wallet does
 * ({@code POST <url>/auth/login}, form field {@code apikey}). Always through Tor. The tokens that
 * come back are never read or kept: only the outcome matters. Used before a Dojo replaces a
 * working one.
 */
public final class HelperDojoLogin {
    private static final int TIMEOUT_MILLIS = 60_000;

    private HelperDojoLogin() {}

    public enum Outcome { OK, BAD_KEY, UNREACHABLE, UNEXPECTED }

    public record Result(Outcome outcome, String message) {
        public boolean ok() {
            return outcome == Outcome.OK;
        }
    }

    /** What the Dojo's answer means. */
    static Result interpret(int status, String body) {
        if (status == 200 && body != null && body.contains("authorizations")) {
            return new Result(Outcome.OK, "Connected: the Dojo accepted the API key.");
        }
        if (status == 401 || status == 403) {
            return new Result(Outcome.BAD_KEY, "The Dojo reached, but it does not accept this API key.");
        }
        return new Result(Outcome.UNEXPECTED, "The Dojo answered " + status + ", which is not a login.");
    }

    /** The login address for a pairing URL, or null if the URL is not usable. */
    static String loginUrl(String dojoUrl) {
        if (dojoUrl == null) {
            return null;
        }
        String trimmed = dojoUrl.trim();
        String lower = trimmed.toLowerCase(Locale.ROOT);
        if (!lower.startsWith("http://") && !lower.startsWith("https://")) {
            return null;
        }
        return trimmed.replaceAll("/+$", "") + "/auth/login";
    }

    /** Blocking (up to about a minute over Tor): call off the UI thread. Never throws. */
    public static Result test(String dojoUrl, String apiKey, int socksPort) {
        String login = loginUrl(dojoUrl);
        if (login == null || apiKey == null || apiKey.isBlank()) {
            return new Result(Outcome.UNEXPECTED, "Enter the Dojo's URL (http:// or https://) and API key first.");
        }
        HttpURLConnection connection = null;
        try {
            Proxy tor = new Proxy(Proxy.Type.SOCKS, new InetSocketAddress("127.0.0.1", socksPort));
            connection = (HttpURLConnection) URI.create(login).toURL().openConnection(tor);
            connection.setRequestMethod("POST");
            connection.setConnectTimeout(TIMEOUT_MILLIS);
            connection.setReadTimeout(TIMEOUT_MILLIS);
            connection.setDoOutput(true);
            connection.setRequestProperty("Content-Type", "application/x-www-form-urlencoded");
            byte[] body = ("apikey=" + URLEncoder.encode(apiKey.trim(), StandardCharsets.UTF_8)).getBytes(StandardCharsets.UTF_8);
            try (OutputStream out = connection.getOutputStream()) {
                out.write(body);
            }
            int status = connection.getResponseCode();
            InputStream stream = status >= 400 ? connection.getErrorStream() : connection.getInputStream();
            String text = stream == null ? "" : new String(stream.readNBytes(64 * 1024), StandardCharsets.UTF_8);
            return interpret(status, text);
        } catch (IOException e) {
            return new Result(Outcome.UNREACHABLE, "Could not reach the Dojo: " + HelperDojoDirectoryLoader.reason(e) + ".");
        } catch (RuntimeException e) {
            return new Result(Outcome.UNEXPECTED, "Could not use that URL.");
        } finally {
            if (connection != null) {
                connection.disconnect();
            }
        }
    }
}
