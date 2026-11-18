package swap.helper;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Proxy;
import java.net.Socket;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.cert.X509Certificate;
import java.util.HexFormat;
import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManager;
import javax.net.ssl.X509TrustManager;
import org.json.JSONObject;

/**
 * Asks an Electrum server three questions through Tor: its version, its chain tip, and the first
 * block's header. The header must hash to the right network's genesis block, so a server on another
 * chain (or an impostor) is rejected; the tip lets the caller drop servers that are behind. This
 * is what decides whether a server found in the Dojo Bay directory may be offered at all.
 */
public final class HelperElectrumProbe {
    public static final String MAINNET_GENESIS = "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f";
    public static final String TESTNET4_GENESIS = "00000000da84f2bafbbc53dee25a72ae507ff4914b867c565be350b0da8bf043";

    private HelperElectrumProbe() {}

    public record Result(String url, boolean ok, long millis, int height, String reason) {}

    /** How a connection to host:port is opened. In the app it is always through Tor. */
    public interface Connector {
        Socket open(String host, int port, boolean tls, int timeoutMillis) throws IOException;
    }

    /**
     * Connections through the Tor SOCKS proxy at {@code socksPort}. The host name goes to the proxy
     * unresolved, so nothing is looked up outside Tor; there is no direct fallback. TLS (on onion
     * services) does not check certificates: an onion address authenticates itself.
     */
    public static Connector throughTor(int socksPort) {
        return (host, port, tls, timeoutMillis) -> {
            Socket socket = new Socket(new Proxy(Proxy.Type.SOCKS, new InetSocketAddress("127.0.0.1", socksPort)));
            try {
                socket.connect(InetSocketAddress.createUnresolved(host, port), timeoutMillis);
                socket.setSoTimeout(timeoutMillis);
                if (!tls) {
                    return socket;
                }
                SSLContext context = SSLContext.getInstance("TLS");
                context.init(null, new TrustManager[] {new X509TrustManager() {
                    public void checkClientTrusted(X509Certificate[] chain, String authType) {}
                    public void checkServerTrusted(X509Certificate[] chain, String authType) {}
                    public X509Certificate[] getAcceptedIssuers() { return new X509Certificate[0]; }
                }}, null);
                return context.getSocketFactory().createSocket(socket, host, port, true);
            } catch (Exception e) {
                socket.close();
                throw e instanceof IOException io ? io : new IOException(e);
            }
        };
    }

    public static Result probe(String url, boolean testnet, Connector connector, int timeoutMillis) {
        long started = System.currentTimeMillis();
        try {
            URI uri = URI.create(url);
            boolean tls = "ssl".equalsIgnoreCase(uri.getScheme());
            if (uri.getHost() == null || uri.getPort() < 0) {
                return new Result(url, false, 0, 0, "not host:port");
            }
            try (Socket socket = connector.open(uri.getHost(), uri.getPort(), tls, timeoutMillis)) {
                OutputStream out = socket.getOutputStream();
                BufferedReader in = new BufferedReader(new InputStreamReader(socket.getInputStream(), StandardCharsets.UTF_8));

                call(out, in, 1, "server.version", "[\"outpost-probe\",\"1.4\"]");
                int height = call(out, in, 2, "blockchain.headers.subscribe", "[]").getJSONObject("result").getInt("height");
                String header = call(out, in, 3, "blockchain.block.header", "[0]").getString("result");
                String genesis = genesisHash(header);
                String wanted = testnet ? TESTNET4_GENESIS : MAINNET_GENESIS;
                long millis = System.currentTimeMillis() - started;
                if (!wanted.equals(genesis)) {
                    return new Result(url, false, millis, height, "another chain");
                }
                return new Result(url, true, millis, height, "");
            }
        } catch (Exception e) {
            String reason = e instanceof java.net.SocketTimeoutException ? "timed out" : e.getMessage() == null ? e.getClass().getSimpleName() : e.getMessage();
            return new Result(url, false, System.currentTimeMillis() - started, 0, reason);
        }
    }

    private static JSONObject call(OutputStream out, BufferedReader in, int id, String method, String params) throws IOException {
        out.write(("{\"id\":" + id + ",\"method\":\"" + method + "\",\"params\":" + params + "}\n").getBytes(StandardCharsets.UTF_8));
        out.flush();
        String line = in.readLine();
        if (line == null) {
            throw new IOException("closed");
        }
        JSONObject reply = new JSONObject(line);
        if (reply.has("error") && !reply.isNull("error")) {
            throw new IOException("server error");
        }
        return reply;
    }

    /** The block hash of an 80-byte header given as hex: double SHA-256, displayed reversed. */
    static String genesisHash(String headerHex) throws Exception {
        byte[] header = HexFormat.of().parseHex(headerHex.substring(0, Math.min(160, headerHex.length())));
        MessageDigest sha = MessageDigest.getInstance("SHA-256");
        byte[] hash = sha.digest(sha.digest(header));
        for (int i = 0; i < hash.length / 2; i++) {
            byte swap = hash[i];
            hash[i] = hash[hash.length - 1 - i];
            hash[hash.length - 1 - i] = swap;
        }
        return HexFormat.of().formatHex(hash);
    }
}
