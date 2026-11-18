package swap.helper;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.attribute.PosixFilePermissions;
import org.junit.Rule;
import org.junit.Test;
import org.junit.rules.TemporaryFolder;

public class HelperSwapDatabaseFileTest {
    @Rule
    public TemporaryFolder folder = new TemporaryFolder();

    @Test
    public void aNewProfileGetsAnEmptyOwnerOnlyDatabaseFile() throws Exception {
        Path dir = folder.getRoot().toPath();

        HelperProperties.ensureSwapDatabaseFile(dir);

        Path database = dir.resolve("sqlite");
        assertTrue(Files.isRegularFile(database));
        assertEquals(0, Files.size(database));
        assertEquals("rw-------", PosixFilePermissions.toString(Files.getPosixFilePermissions(database)));
    }

    @Test
    public void anExistingDatabaseIsNeverTouched() throws Exception {
        Path dir = folder.getRoot().toPath();
        Path database = dir.resolve("sqlite");
        byte[] content = "SQLite format 3\0real swaps".getBytes();
        Files.write(database, content);

        HelperProperties.ensureSwapDatabaseFile(dir);
        HelperProperties.ensureSwapDatabaseFile(dir);

        assertArrayEquals(content, Files.readAllBytes(database));
    }
}
