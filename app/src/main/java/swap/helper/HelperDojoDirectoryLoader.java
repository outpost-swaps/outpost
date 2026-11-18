package swap.helper;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.IOException;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.InetSocketAddress;
import java.net.Proxy;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.StandardCopyOption;

/**
 * Where the public-Dojo list comes from, best first:
 * <ol>
 *   <li><b>Live</b>: Dojo Bay's directory fetched over Tor (never over the clearnet), the way
 *       Ashigaru Desktop does it, with every entry's signature checked.
 *   <li><b>Saved</b>: the last live copy, kept in the data folder and verified again every time it
 *       is read, so a changed file cannot slip an entry in.
 *   <li><b>Shipped</b>: the copy that came with the app, the offline fallback (verified the same way,
 *       but out of date by the time anyone reads it).
 * </ol>
 */
public final class HelperDojoDirectoryLoader {
    public static final String DIRECTORY_URL = "http://dojobayeryasshgghz537de5ckgd5hhi4z5sdeil3roeh65fwhdnu2yd.onion/data/dojos.json";
    private static final int MAX_BYTES = 4 * 1024 * 1024;
    private static final int TIMEOUT_MILLIS = 90_000;

    private HelperDojoDirectoryLoader() {}

    public enum Source { LIVE, SAVED, SHIPPED }

    public record Loaded(HelperDojoDirectory.Directory directory, Source source) {}

    /** The saved copy if there is a good one at least as new as the shipped copy, otherwise the shipped copy. Instant, no network. */
    public static Loaded loadOffline(File cacheFile) {
        HelperDojoDirectory.Directory shipped = HelperDojoDirectory.bundled();
        HelperDojoDirectory.Directory saved = readSaved(cacheFile);
        if (saved != null && !saved.entries().isEmpty()
                && (shipped.generatedAt() == null || saved.generatedAt() != null && saved.generatedAt().compareTo(shipped.generatedAt()) >= 0)) {
            return new Loaded(saved, Source.SAVED);
        }
        return new Loaded(shipped, Source.SHIPPED);
    }

    /**
     * Fetches the directory through the Tor SOCKS proxy at {@code socksPort}, keeps only the entries
     * that verify, and saves the copy. Blocking: call off the UI thread.
     *
     * @throws IOException when the directory cannot be reached or nothing in it verifies
     */
    public static Loaded loadLive(int socksPort, File cacheFile) throws IOException {
        Loaded recent = lastLive;
        if (recent != null && System.currentTimeMillis() - lastLiveAtMillis < LIVE_REUSE_MILLIS) {
            return recent; // the Dojo list and the Electrum discovery share one fetch
        }
        String json = fetch(socksPort);
        HelperDojoDirectory.Directory directory = HelperDojoDirectory.parse(json);
        if (directory.entries().isEmpty()) {
            throw new IOException("the directory had no entry whose signature checks out");
        }
        save(cacheFile, json);
        Loaded loaded = new Loaded(directory, Source.LIVE);
        lastLive = loaded;
        lastLiveAtMillis = System.currentTimeMillis();
        return loaded;
    }

    private static final long LIVE_REUSE_MILLIS = 10 * 60 * 1000L;
    private static volatile Loaded lastLive;
    private static volatile long lastLiveAtMillis;

    /** For tests. */
    static void forgetLive() {
        lastLive = null;
        lastLiveAtMillis = 0;
    }

    /** One GET through Tor. The hostname goes to the proxy unresolved, so nothing is looked up outside Tor. */
    static String fetch(int socksPort) throws IOException {
        Proxy tor = new Proxy(Proxy.Type.SOCKS, new InetSocketAddress("127.0.0.1", socksPort));
        HttpURLConnection connection = (HttpURLConnection) URI.create(DIRECTORY_URL).toURL().openConnection(tor);
        connection.setConnectTimeout(TIMEOUT_MILLIS);
        connection.setReadTimeout(TIMEOUT_MILLIS);
        connection.setRequestProperty("Accept", "application/json");
        try {
            int status = connection.getResponseCode();
            if (status != 200) {
                throw new IOException("Dojo Bay answered " + status);
            }
            try (InputStream in = connection.getInputStream()) {
                return readLimited(in, MAX_BYTES);
            }
        } finally {
            connection.disconnect();
        }
    }

    static String readLimited(InputStream in, int maxBytes) throws IOException {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        byte[] buffer = new byte[8192];
        int read;
        while ((read = in.read(buffer)) != -1) {
            out.write(buffer, 0, read);
            if (out.size() > maxBytes) {
                throw new IOException("the directory is larger than expected");
            }
        }
        return out.toString(StandardCharsets.UTF_8);
    }

    static HelperDojoDirectory.Directory readSaved(File cacheFile) {
        try {
            if (cacheFile == null || !cacheFile.isFile() || cacheFile.length() > MAX_BYTES) {
                return null;
            }
            return HelperDojoDirectory.parse(new String(Files.readAllBytes(cacheFile.toPath()), StandardCharsets.UTF_8));
        } catch (Exception e) {
            return null;
        }
    }

    /** Replaces the saved copy in one step, so a crash never leaves half a file. */
    static void save(File cacheFile, String json) {
        if (cacheFile == null) {
            return;
        }
        try {
            File temp = new File(cacheFile.getParentFile(), cacheFile.getName() + ".tmp");
            Files.write(temp.toPath(), json.getBytes(StandardCharsets.UTF_8));
            Files.move(temp.toPath(), cacheFile.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
        } catch (Exception e) {
            // not being able to save only means the next start loads the shipped copy first
        }
    }

    /** A short, plain reason for the window ("timed out", ...). */
    public static String reason(Exception e) {
        String message = e.getMessage();
        if (e instanceof java.net.SocketTimeoutException) {
            return "timed out";
        }
        return message == null || message.isBlank() ? e.getClass().getSimpleName() : message;
    }
}
