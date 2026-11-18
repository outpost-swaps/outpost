package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import org.junit.Test;

/** Runs the probe against a small fake Electrum server on this machine (the connector is injected; the app only uses Tor). */
public class HelperElectrumProbeTest {
    // the real mainnet and testnet4 genesis block headers (80 bytes)
    private static final String MAINNET_HEADER = "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c";
    private static final String TESTNET4_HEADER = "0100000000000000000000000000000000000000000000000000000000000000000000004e7b2b9128fe0291db0693af2ae418b767e657cd407e80cb1434221eaea7a07a046f3566ffff001dbb0c78";

    private interface Behaviour {
        void serve(BufferedReader in, OutputStream out) throws IOException;
    }

    private static String reply(int id, String result) {
        return "{\"jsonrpc\":\"2.0\",\"id\":" + id + ",\"result\":" + result + "}\n";
    }

    private static Behaviour honest(String header, int height) {
        return (in, out) -> {
            in.readLine();
            out.write(reply(1, "[\"Fake 1.0\",\"1.4\"]").getBytes(StandardCharsets.UTF_8));
            in.readLine();
            out.write(reply(2, "{\"height\":" + height + ",\"hex\":\"00\"}").getBytes(StandardCharsets.UTF_8));
            in.readLine();
            out.write(reply(3, "\"" + header + "\"").getBytes(StandardCharsets.UTF_8));
            out.flush();
        };
    }

    private static HelperElectrumProbe.Result run(Behaviour behaviour, boolean testnet) throws Exception {
        try (ServerSocket server = new ServerSocket(0)) {
            Thread thread = new Thread(() -> {
                try (Socket client = server.accept()) {
                    behaviour.serve(new BufferedReader(new InputStreamReader(client.getInputStream(), StandardCharsets.UTF_8)), client.getOutputStream());
                } catch (IOException ignored) {
                    // the probe hung up
                }
            });
            thread.setDaemon(true);
            thread.start();
            HelperElectrumProbe.Connector direct = (host, port, tls, timeout) -> {
                Socket socket = new Socket("127.0.0.1", server.getLocalPort());
                socket.setSoTimeout(timeout);
                return socket;
            };
            return HelperElectrumProbe.probe("tcp://fake.onion:50001", testnet, direct, 3000);
        }
    }

    @Test
    public void theGenesisHashesOfTheRealHeadersAreWhatTheProbeExpects() throws Exception {
        assertEquals(HelperElectrumProbe.MAINNET_GENESIS, HelperElectrumProbe.genesisHash(MAINNET_HEADER));
    }

    @Test
    public void anHonestMainnetServerPassesAndReportsItsTip() throws Exception {
        HelperElectrumProbe.Result result = run(honest(MAINNET_HEADER, 975189), false);

        assertTrue(result.reason(), result.ok());
        assertEquals(975189, result.height());
    }

    @Test
    public void aServerOnAnotherChainIsRejected() throws Exception {
        HelperElectrumProbe.Result result = run(honest(MAINNET_HEADER, 975189), true);

        assertFalse(result.ok());
        assertEquals("another chain", result.reason());
    }

    @Test
    public void aServerThatHangsUpOrErrorsIsRejectedWithAReason() throws Exception {
        assertFalse(run((in, out) -> { }, false).ok());
        HelperElectrumProbe.Result error = run((in, out) -> {
            in.readLine();
            out.write("{\"id\":1,\"error\":{\"code\":-1}}\n".getBytes(StandardCharsets.UTF_8));
            out.flush();
        }, false);
        assertFalse(error.ok());
        assertEquals("server error", error.reason());
    }

    @Test
    public void aMalformedUrlIsRejectedWithoutConnecting() {
        HelperElectrumProbe.Result result = HelperElectrumProbe.probe("nonsense", false, (h, p, t, to) -> { throw new IOException("must not connect"); }, 1000);

        assertFalse(result.ok());
    }
}
