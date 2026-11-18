package swap.lib;

import org.bitcoinj.core.NetworkParameters;
import org.bitcoinj.params.MainNetParams;
import org.bitcoinj.params.TestNet3Params;
import org.json.JSONObject;
import swap.helper.HelperNetwork;
import swap.helper.HelperProperties;
import swap.listener.RpcDownloadListener;
import swap.process.ProcessRpcMonero;

import java.io.File;
import java.io.IOException;
import java.net.ServerSocket;
import java.security.SecureRandom;
import java.util.ArrayList;
import java.util.HexFormat;
import java.util.Random;

public class AppXmrRpc {
    public static final String RPC_SWAPCLIENT = "rpc_swapclient";
    public static final String RPC_ASB = "rpc_asb";

    // TODO convert to a swap.lib.App
    public static ProcessRpcMonero startMoneroRpc(NetworkParameters params, String rpcFilePath, String daemonAddress, int torPort, String rpcName) {
        return startMoneroRpc(params, rpcFilePath, daemonAddress, torPort, rpcName, false, daemonAddress);
    }

    /**
     * @param daemonViaLocalForwarder true when daemonAddress is the local
     *                                daemon-pool forwarder (which dials the
     *                                real daemons through Tor itself, so the
     *                                wallet RPC must not add --proxy)
     * @param trustSourceAddress      the address the trusted-daemon decision
     *                                is based on (the real daemon, not the
     *                                127.0.0.1 forwarder)
     */
    public static ProcessRpcMonero startMoneroRpc(NetworkParameters params, String rpcFilePath, String daemonAddress, int torPort, String rpcName, boolean daemonViaLocalForwarder, String trustSourceAddress) {
        if (HelperProperties.isUseTor() && !daemonViaLocalForwarder && torPort == AppSwap.UNINITIALIZED_TOR_PORT) {
            throw new IllegalStateException("Tor is enabled but no SOCKS proxy port is available; refusing to start monero-wallet-rpc without Tor");
        }

        int port = generatePort();
        String previousPid = HelperProperties.getProperty(rpcName + "_pid");
        if (previousPid != null && !previousPid.isEmpty()) {
            ProcessRpcMonero.shutdown(previousPid);
        }
        String network = HelperNetwork.isTestnet(params) ? "testnet" : "mainnet";
        if (RPC_SWAPCLIENT.equals(rpcName)) {
            AppSwap.deleteStaleMonitoringWalletFiles(new File(rpcFilePath, "monero-data/" + network));
        }
        String rpcUser = randomCredential();
        String rpcPassword = randomCredential();
        deleteStaleLoginFiles(rpcFilePath);
        try {
            Process process = new ProcessBuilder(buildCommand(
                    params, rpcFilePath, daemonAddress, torPort,
                    daemonViaLocalForwarder, trustSourceAddress, port,
                    rpcUser, rpcPassword)).inheritIO().start();
            HelperProperties.setProperty(rpcName + "_pid", process.pid() + "");
            return new ProcessRpcMonero(process, endpoint(rpcUser, rpcPassword, port));
        } catch (IOException e) {
            throw new RuntimeException(e);
        }
    }

    /**
     * Restarts a wallet RPC that exited, reusing the existing port and
     * credentials so the endpoint already handed to the swap engine keeps
     * working. Startup cleanup is deliberately skipped: the wallet files are
     * in use by the running swap.
     */
    public static ProcessRpcMonero restartMoneroRpc(
            ProcessRpcMonero previous, NetworkParameters params, String rpcFilePath,
            String daemonAddress, int torPort, boolean daemonViaLocalForwarder,
            String trustSourceAddress) {
        try {
            RpcEndpoint parsed = parseEndpoint(previous.endpoint());
            Process process = new ProcessBuilder(buildCommand(
                    params, rpcFilePath, daemonAddress, torPort,
                    daemonViaLocalForwarder, trustSourceAddress, parsed.port(),
                    parsed.user(), parsed.password())).inheritIO().start();
            HelperProperties.setProperty(RPC_SWAPCLIENT + "_pid", process.pid() + "");
            return new ProcessRpcMonero(process, previous.endpoint());
        } catch (Exception e) {
            throw new RuntimeException(e);
        }
    }

    /** Port and credentials of an endpoint handed to the swap engine. */
    public record RpcEndpoint(int port, String user, String password) {}

    /**
     * Parses `http://user:pass@127.0.0.1:port/json_rpc`. A restart must reuse
     * exactly these values: the engine holds this endpoint for the whole swap
     * and cannot be told about a new port.
     */
    static RpcEndpoint parseEndpoint(String endpoint) {
        java.net.URI uri = java.net.URI.create(endpoint);
        String[] credentials = uri.getUserInfo().split(":", 2);
        return new RpcEndpoint(uri.getPort(), credentials[0], credentials[1]);
    }

    private static String endpoint(String rpcUser, String rpcPassword, int port) {
        return "http://" + rpcUser + ":" + rpcPassword + "@127.0.0.1:" + port + "/json_rpc";
    }

    static ArrayList<String> buildCommand(
            NetworkParameters params, String rpcFilePath, String daemonAddress,
            int torPort, boolean daemonViaLocalForwarder, String trustSourceAddress,
            int port, String rpcUser, String rpcPassword) {
        ArrayList<String> cmd = new ArrayList<>();
        cmd.add(rpcFilePath + "/monero-wallet-rpc");
        cmd.add("--daemon-address");
        cmd.add(daemonAddress);
        cmd.add("--rpc-bind-port");
        cmd.add(String.valueOf(port));
        // The login goes in a file only the owner can read, not on the command line: arguments
        // are visible to every local user through `ps`.
        cmd.add("--config-file");
        cmd.add(writeLoginFile(rpcFilePath, port, rpcUser, rpcPassword).toString());
        cmd.add("--wallet-dir");
        String network = HelperNetwork.isTestnet(params) ? "testnet" : "mainnet";
        cmd.add(rpcFilePath + "/monero-data/" + network);
        if (HelperNetwork.isTestnet(params)) {
            cmd.add("--stagenet");
        }

        // Scan-speed: the wallet RPC defaults to untrusted-daemon mode,
        // which uses small block batches plus extra verification
        // round-trips per batch. Over a Tor onion daemon every
        // round-trip costs seconds, so the redeem wallet's first sync
        // (swap_state_manager's create_from_and_load + refresh) takes
        // 10+ minutes. Trust the daemon when it is the test networks'
        // default (no real funds at stake) or when it is the user's own
        // LOCAL node (localhost) — a local monerod is exactly the
        // "trusted daemon" case, so mainnet users running their own node
        // get the fast scan too. Remote mainnet daemons keep the
        // conservative untrusted default unless the user opts in via the
        // settings toggle.
        boolean daemonIsLocal = trustSourceAddress != null
                && (trustSourceAddress.contains("127.0.0.1") || trustSourceAddress.contains("localhost"));
        boolean trustedDaemon = HelperNetwork.isTestnet(params)
                || daemonIsLocal
                || HelperProperties.isTrustDaemonInEffect();
        if (trustedDaemon) {
            cmd.add("--trusted-daemon");
        }

        if (!daemonViaLocalForwarder && torPort != AppSwap.UNINITIALIZED_TOR_PORT) {
            cmd.add("--proxy");
            cmd.add("127.0.0.1:" + torPort);
            cmd.add("--daemon-ssl-allow-any-cert");
        }

        return cmd;
    }

    private static final String LOGIN_FILE_PREFIX = "wallet-rpc-login-";
    private static final String LOGIN_FILE_SUFFIX = ".conf";

    /** Writes `rpc-login=user:password` for monero-wallet-rpc's `--config-file` (mode 0600). */
    static java.nio.file.Path writeLoginFile(String rpcFilePath, int port, String user, String password) {
        java.nio.file.Path file = java.nio.file.Path.of(rpcFilePath, LOGIN_FILE_PREFIX + port + LOGIN_FILE_SUFFIX);
        try {
            java.nio.file.Files.deleteIfExists(file);
            try {
                java.nio.file.Files.createFile(
                        file,
                        java.nio.file.attribute.PosixFilePermissions.asFileAttribute(
                                java.nio.file.attribute.PosixFilePermissions.fromString("rw-------")));
            } catch (UnsupportedOperationException e) {
                java.nio.file.Files.createFile(file); // no POSIX modes (Windows)
            }
            java.nio.file.Files.writeString(file, "rpc-login=" + user + ":" + password + "\n");
            return file;
        } catch (IOException e) {
            throw new java.io.UncheckedIOException("could not write the wallet RPC login file", e);
        }
    }

    /** Removes login files left by earlier runs; the running RPCs read theirs at start-up. */
    static void deleteStaleLoginFiles(String rpcFilePath) {
        File[] stale = new File(rpcFilePath).listFiles((dir, name) ->
                name.startsWith(LOGIN_FILE_PREFIX) && name.endsWith(LOGIN_FILE_SUFFIX));
        if (stale != null) {
            for (File file : stale) {
                file.delete();
            }
        }
    }

    private static String randomCredential() {
        byte[] bytes = new byte[16];
        new SecureRandom().nextBytes(bytes);
        return HexFormat.of().formatHex(bytes);
    }

    private static int generatePort() {
        int port = -1;
        while (port == -1) {
            int tempPort = new Random().nextInt(60000) + 5000;
            try {
                new ServerSocket(tempPort).close();
                // finds open port between 5000-60000
                port = tempPort;
            } catch (IOException ignored) {
            }
        }
        return port;
    }

    /**
     * Waits until the wallet RPC is actually accepting connections. The process
     * binds its port at launch but refuses connections until the RPC server
     * starts serving (the daemon handshake through the Tor forwarder can take
     * several seconds), so a fixed sleep is both too short and too long.
     * Returns true when a probe connected, false on timeout.
     */
    public static boolean waitUntilListening(ProcessRpcMonero rpc, long timeoutMillis) {
        int port = java.net.URI.create(rpc.endpoint()).getPort();
        long deadline = System.currentTimeMillis() + timeoutMillis;
        while (System.currentTimeMillis() < deadline) {
            try (java.net.Socket probe = new java.net.Socket("127.0.0.1", port)) {
                return true;
            } catch (IOException ignored) {
                try {
                    Thread.sleep(250L);
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                    return false;
                }
            }
        }
        return false;
    }

    public static File maybeDownloadXmrRpc(String proxy, RpcDownloadListener rpcDownloadListener) {
        JSONObject downloadRpcResponseJson = new JSONObject(maybeDownloadXmrRpc(proxy));
        String error = downloadRpcResponseJson.getString("error");
        String path = downloadRpcResponseJson.getString("rpcPath");
        if (error.isEmpty()) {
            if (rpcDownloadListener != null) {
                rpcDownloadListener.onRpcDownloaded();
            }
        } else {
            if (rpcDownloadListener != null) {
                rpcDownloadListener.onRpcDownloadError(error);
            }
        }

        return new File(path);
    }

    // returns file path where monero-wallet-rpc was downloaded
    private static native String maybeDownloadXmrRpc(String proxy);
}
