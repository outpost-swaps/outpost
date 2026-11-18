package swap.helper;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.Socket;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import org.json.JSONObject;

/**
 * Checks one Monero node before it is used: asks {@code /get_info} over the same Tor connector as
 * {@link HelperElectrumProbe} and accepts the node only when it answers OK on the expected network
 * (mainnet, or stagenet for a testnet wallet).
 */
public final class HelperMoneroProbe {
    private HelperMoneroProbe() {}

    public record Result(String url, boolean ok, long millis, long height, String reason) {}

    private static final int MAX_RESPONSE_BYTES = 64 * 1024;

    public static Result probe(String url, boolean stagenet, HelperElectrumProbe.Connector connector, int timeoutMillis) {
        long started = System.currentTimeMillis();
        try {
            URI uri = URI.create(url);
            if (uri.getHost() == null || uri.getPort() < 0) {
                return new Result(url, false, 0, 0, "not host:port");
            }
            boolean tls = "https".equalsIgnoreCase(uri.getScheme());
            try (Socket socket = connector.open(uri.getHost(), uri.getPort(), tls, timeoutMillis)) {
                String body = "{\"jsonrpc\":\"2.0\",\"id\":\"0\",\"method\":\"get_info\"}";
                String request = "POST /json_rpc HTTP/1.1\r\nHost: " + uri.getHost() + "\r\nContent-Type: application/json\r\n"
                        + "Content-Length: " + body.getBytes(StandardCharsets.UTF_8).length + "\r\nConnection: close\r\n\r\n" + body;
                OutputStream out = socket.getOutputStream();
                out.write(request.getBytes(StandardCharsets.UTF_8));
                out.flush();
                return read(url, stagenet, socket.getInputStream(), started);
            }
        } catch (Exception e) {
            String reason = e instanceof java.net.SocketTimeoutException ? "timed out" : e.getMessage() == null ? e.getClass().getSimpleName() : e.getMessage();
            return new Result(url, false, System.currentTimeMillis() - started, 0, reason);
        }
    }

    /** Reads an HTTP response to {@code get_info} and judges it (separate so tests can feed bytes). */
    static Result read(String url, boolean stagenet, InputStream in, long started) throws Exception {
        ByteArrayOutputStream buffer = new ByteArrayOutputStream();
        byte[] chunk = new byte[4096];
        int read;
        while ((read = in.read(chunk)) != -1) {
            buffer.write(chunk, 0, read);
            if (buffer.size() > MAX_RESPONSE_BYTES) {
                return new Result(url, false, System.currentTimeMillis() - started, 0, "answer too large");
            }
        }
        long millis = System.currentTimeMillis() - started;
        String response = buffer.toString(StandardCharsets.UTF_8);
        int split = response.indexOf("\r\n\r\n");
        if (!response.startsWith("HTTP/1.") || split < 0) {
            return new Result(url, false, millis, 0, "not a Monero node");
        }
        String status = response.substring(0, response.indexOf("\r\n"));
        if (!status.contains(" 200")) {
            return new Result(url, false, millis, 0, "answered " + status.substring(Math.min(9, status.length())).trim());
        }
        JSONObject result;
        try {
            result = new JSONObject(response.substring(split + 4).trim()).getJSONObject("result");
        } catch (Exception e) {
            return new Result(url, false, millis, 0, "not a Monero node");
        }
        if (!"OK".equals(result.optString("status"))) {
            return new Result(url, false, millis, 0, "status " + result.optString("status", "missing"));
        }
        String nettype = result.optString("nettype", "");
        String wanted = stagenet ? "stagenet" : "mainnet";
        long height = result.optLong("height", 0);
        if (!nettype.isEmpty() && !nettype.equals(wanted)) {
            return new Result(url, false, millis, height, "a " + nettype + " node, not " + wanted);
        }
        if (result.optBoolean("synchronized", true) == false) {
            return new Result(url, false, millis, height, "still syncing");
        }
        return new Result(url, true, millis, height, "");
    }
}
