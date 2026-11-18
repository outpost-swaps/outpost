package swap.helper;

import org.junit.Test;

import java.util.List;
import java.util.Map;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

public class HelperBuildManifestTest {
    private static final String MANIFEST = """
            # written by the prepare script
            build_id=20261107T230000Z-abc123-def456
            asb=aaaa1111
            libatomicswap.dylib=bbbb2222

            daemon_pool_proxy=cccc3333
            """;

    @Test
    public void parsesEntriesAndIgnoresCommentsAndBlankLines() {
        Map<String, String> manifest = HelperBuildManifest.parse(MANIFEST);
        assertEquals(4, manifest.size());
        assertEquals("aaaa1111", manifest.get("asb"));
        assertEquals("20261107T230000Z-abc123-def456", manifest.get("build_id"));
    }

    @Test
    public void matchingArtifactsHaveNoProblems() {
        Map<String, String> manifest = HelperBuildManifest.parse(MANIFEST);
        List<String> problems = HelperBuildManifest.mismatches(manifest, key -> manifest.get(key));
        assertTrue(problems.toString(), problems.isEmpty());
    }

    // The 7207846b case: the library was replaced, the asb was not.
    @Test
    public void aReplacedLibraryNextToAnOldAsbIsReported() {
        Map<String, String> manifest = HelperBuildManifest.parse(MANIFEST);
        List<String> problems = HelperBuildManifest.mismatches(manifest,
                key -> key.equals("libatomicswap.dylib") ? "ffff9999" : manifest.get(key));
        assertEquals(1, problems.size());
        assertTrue(problems.get(0), problems.get(0).startsWith("libatomicswap.dylib does not match"));
    }

    @Test
    public void aMissingArtifactIsReported() {
        Map<String, String> manifest = HelperBuildManifest.parse(MANIFEST);
        List<String> problems = HelperBuildManifest.mismatches(manifest,
                key -> key.equals("asb") ? null : manifest.get(key));
        assertEquals(1, problems.size());
        assertTrue(problems.get(0), problems.get(0).contains("missing from the app"));
    }

    @Test
    public void librariesLiveAtTheRootAndProgramsUnderBinaries() {
        assertEquals("/libatomicswap.dylib", HelperBuildManifest.resourcePathFor("libatomicswap.dylib"));
        assertEquals("/libatomicswap.so", HelperBuildManifest.resourcePathFor("libatomicswap.so"));
        assertEquals("/binaries/asb", HelperBuildManifest.resourcePathFor("asb"));
        assertEquals("/binaries/daemon_pool_proxy", HelperBuildManifest.resourcePathFor("daemon_pool_proxy"));
    }
}
