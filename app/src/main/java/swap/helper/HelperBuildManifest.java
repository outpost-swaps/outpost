package swap.helper;

import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.function.Function;

/**
 * Checks that the native library and the bundled programs (asb, the daemon pool
 * proxy, the price ticker) come from the same build.
 *
 * `scripts/prepare/write-build-manifest.sh` records the SHA-256 of each artifact
 * right after the prepare script builds them. A later rebuild of only one piece
 * (for example copying a fresh `libatomicswap` next to an old `asb`) no longer
 * matches the manifest, and the app refuses to start the ASB with a clear message
 * instead of failing mid-swap: swap `7207846b` failed because the library and the
 * `asb` derived fees differently, and nothing on the wire carries that.
 */
public final class HelperBuildManifest {
    static final String MANIFEST_RESOURCE = "/binaries/build-manifest.txt";
    private static final String BUILD_ID_KEY = "build_id";
    private static volatile boolean verified = false;

    private HelperBuildManifest() {
    }

    /** Parses {@code key=value} lines, ignoring blanks and {@code #} comments. */
    static Map<String, String> parse(String text) {
        Map<String, String> entries = new LinkedHashMap<>();
        if (text == null) {
            return entries;
        }
        for (String line : text.split("\\R")) {
            String trimmed = line.trim();
            if (trimmed.isEmpty() || trimmed.startsWith("#")) {
                continue;
            }
            int eq = trimmed.indexOf('=');
            if (eq <= 0) {
                continue;
            }
            entries.put(trimmed.substring(0, eq).trim(), trimmed.substring(eq + 1).trim());
        }
        return entries;
    }

    /** The artifacts whose hash differs from the manifest, as readable lines. */
    static List<String> mismatches(Map<String, String> manifest, Function<String, String> actualHash) {
        List<String> problems = new ArrayList<>();
        for (Map.Entry<String, String> entry : manifest.entrySet()) {
            if (entry.getKey().equals(BUILD_ID_KEY)) {
                continue;
            }
            String actual = actualHash.apply(entry.getKey());
            if (actual == null) {
                problems.add(entry.getKey() + " is listed in the build manifest but missing from the app");
            } else if (!actual.equalsIgnoreCase(entry.getValue())) {
                problems.add(entry.getKey() + " does not match the build manifest (expected "
                        + shorten(entry.getValue()) + ", found " + shorten(actual) + ")");
            }
        }
        return problems;
    }

    private static String shorten(String hash) {
        return hash.length() > 12 ? hash.substring(0, 12) : hash;
    }

    /** The resource a manifest key refers to: libraries at the root, programs under binaries/. */
    static String resourcePathFor(String key) {
        boolean library = key.startsWith("lib") || key.endsWith(".dll");
        return library ? "/" + key : "/binaries/" + key;
    }

    /**
     * Verifies the bundled artifacts once per run. A missing manifest (a build
     * made without the prepare script) only logs; a mismatch throws.
     */
    public static void verifyBundledOnce() {
        if (verified) {
            return;
        }
        String text;
        try (InputStream in = HelperBuildManifest.class.getResourceAsStream(MANIFEST_RESOURCE)) {
            if (in == null) {
                System.out.println("[BUILD] No build manifest in this app; skipping the library/asb match check.");
                verified = true;
                return;
            }
            text = new String(in.readAllBytes(), StandardCharsets.UTF_8);
        } catch (Exception e) {
            throw new IllegalStateException("Could not read the build manifest: " + e.getMessage(), e);
        }

        Map<String, String> manifest = parse(text);
        List<String> problems = mismatches(manifest, key -> hashOfResource(resourcePathFor(key)));
        if (!problems.isEmpty()) {
            throw new IllegalStateException("The app's native library and bundled programs are not from the same build: "
                    + String.join("; ", problems) + ". Run the prepare script again and restart the app.");
        }
        System.out.println("[BUILD] Library and bundled programs match build " + manifest.getOrDefault(BUILD_ID_KEY, "(unknown)"));
        verified = true;
    }

    private static String hashOfResource(String resource) {
        try (InputStream in = HelperBuildManifest.class.getResourceAsStream(resource)) {
            if (in == null) {
                return null;
            }
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            byte[] buffer = new byte[1 << 16];
            int read;
            while ((read = in.read(buffer)) != -1) {
                digest.update(buffer, 0, read);
            }
            StringBuilder hex = new StringBuilder();
            for (byte b : digest.digest()) {
                hex.append(String.format("%02x", b));
            }
            return hex.toString();
        } catch (Exception e) {
            throw new IllegalStateException("Could not hash " + resource + ": " + e.getMessage(), e);
        }
    }
}
