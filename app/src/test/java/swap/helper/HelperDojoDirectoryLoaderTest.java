package swap.helper;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertThrows;
import static org.junit.Assert.assertTrue;

import java.io.ByteArrayInputStream;
import java.io.File;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import org.junit.Rule;
import org.junit.Test;
import org.junit.rules.TemporaryFolder;

public class HelperDojoDirectoryLoaderTest {
    /** generated_at of the shipped dojo-directory.json (the 2026-11-15 snapshot). */
    private static final String SHIPPED_AT = "2026-11-15T00:31:05Z";

    @Rule public TemporaryFolder folder = new TemporaryFolder();

    @Test
    public void withNoSavedCopyTheShippedOneIsUsed() throws Exception {
        HelperDojoDirectoryLoader.Loaded loaded = HelperDojoDirectoryLoader.loadOffline(new File(folder.getRoot(), "missing.json"));

        assertEquals(HelperDojoDirectoryLoader.Source.SHIPPED, loaded.source());
        assertFalse(loaded.directory().entries().isEmpty());
    }

    @Test
    public void aNewerSavedCopyIsPreferredAndItIsVerifiedAgainWhenRead() throws Exception {
        // the shipped file, relabelled as generated later: stands in for a copy saved from a live fetch
        String newer = HelperDojoDirectory.bundledJson().replace(SHIPPED_AT, "2026-12-01T12:00:00Z");
        File cache = folder.newFile("cache.json");
        Files.writeString(cache.toPath(), newer);

        HelperDojoDirectoryLoader.Loaded loaded = HelperDojoDirectoryLoader.loadOffline(cache);

        assertEquals(HelperDojoDirectoryLoader.Source.SAVED, loaded.source());
        assertEquals("2026-12-01", loaded.directory().snapshotDate());
        assertEquals(12, loaded.directory().entries().size());
    }

    @Test
    public void aSavedCopyThatWasAlteredLosesTheAlteredEntries() throws Exception {
        String bundled = HelperDojoDirectory.bundledJson().replace(SHIPPED_AT, "2026-12-01T12:00:00Z");
        // point every pairing URL somewhere else; the signed text still says the original
        String altered = bundled.replace("\"url\": \"http://", "\"url\": \"http://evil")
                .replace("\"url\":\"http://", "\"url\":\"http://evil");
        assertTrue("the alteration must actually change the file", !altered.equals(bundled));
        File cache = folder.newFile("cache.json");
        Files.writeString(cache.toPath(), altered);

        HelperDojoDirectory.Directory directory = HelperDojoDirectoryLoader.readSaved(cache);

        assertTrue(directory.entries().stream().noneMatch(e -> e.url().contains("evil")));
    }

    @Test
    public void aSavedCopyOlderThanTheShippedOneIsIgnored() throws Exception {
        String older = HelperDojoDirectory.bundledJson().replace(SHIPPED_AT, "2026-01-01T00:00:00Z");
        File cache = folder.newFile("cache.json");
        Files.writeString(cache.toPath(), older);

        assertEquals(HelperDojoDirectoryLoader.Source.SHIPPED, HelperDojoDirectoryLoader.loadOffline(cache).source());
    }

    @Test
    public void aBrokenOrEmptySavedFileFallsBackToTheShippedCopy() throws Exception {
        File cache = folder.newFile("cache.json");
        Files.writeString(cache.toPath(), "not json at all");

        assertEquals(HelperDojoDirectoryLoader.Source.SHIPPED, HelperDojoDirectoryLoader.loadOffline(cache).source());
        assertNull(HelperDojoDirectoryLoader.readSaved(new File(folder.getRoot(), "nope.json")));
    }

    @Test
    public void saveWritesTheFileAndLeavesNoTemporaryOne() throws Exception {
        File cache = new File(folder.getRoot(), "dojo-directory-cache.json");

        HelperDojoDirectoryLoader.save(cache, "{\"nodes\":[]}");

        assertEquals("{\"nodes\":[]}", Files.readString(cache.toPath()));
        assertFalse(new File(folder.getRoot(), "dojo-directory-cache.json.tmp").exists());
    }

    @Test
    public void anOversizedAnswerIsRefused() {
        byte[] big = new byte[2000];
        assertThrows(IOException.class, () -> HelperDojoDirectoryLoader.readLimited(new ByteArrayInputStream(big), 1000));
        assertEquals("ab", assertDoesNotThrow(() -> HelperDojoDirectoryLoader.readLimited(new ByteArrayInputStream("ab".getBytes(StandardCharsets.UTF_8)), 1000)));
    }

    private static <T> T assertDoesNotThrow(java.util.concurrent.Callable<T> call) {
        try {
            return call.call();
        } catch (Exception e) {
            throw new AssertionError(e);
        }
    }

    @Test
    public void theReasonIsShortAndPlain() {
        assertEquals("timed out", HelperDojoDirectoryLoader.reason(new java.net.SocketTimeoutException("Read timed out")));
        assertEquals("Dojo Bay answered 503", HelperDojoDirectoryLoader.reason(new IOException("Dojo Bay answered 503")));
    }
}
