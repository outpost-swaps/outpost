package swap.helper;

import org.junit.Rule;
import org.junit.Test;
import org.junit.rules.TemporaryFolder;

import java.io.File;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.LocalDate;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

public class HelperDatabaseBackupTest {
    @Rule
    public TemporaryFolder tmp = new TemporaryFolder();

    @Test
    public void copiesSwapAndAsbDatabasesWithWalFiles() throws Exception {
        File root = tmp.getRoot();
        Files.writeString(root.toPath().resolve("sqlite"), "bob-db");
        Files.writeString(root.toPath().resolve("sqlite-wal"), "bob-wal");
        Files.writeString(root.toPath().resolve("swaps.json"), "{}");
        Files.createDirectories(root.toPath().resolve("asb/testnet"));
        Files.writeString(root.toPath().resolve("asb/testnet/sqlite"), "asb-db");

        HelperDatabaseBackup.backup(root, LocalDate.of(2026, 10, 27));

        Path day = root.toPath().resolve("backups").resolve("20261027");
        assertEquals("bob-db", Files.readString(day.resolve("sqlite")));
        assertEquals("bob-wal", Files.readString(day.resolve("sqlite-wal")));
        assertEquals("{}", Files.readString(day.resolve("swaps.json")));
        assertEquals("asb-db", Files.readString(day.resolve("asb/testnet/sqlite")));
        assertTrue(Files.isRegularFile(day.resolve(".complete")));
    }

    @Test
    public void doesNotRewriteACompleteBackup() throws Exception {
        File root = tmp.getRoot();
        Files.writeString(root.toPath().resolve("sqlite"), "first");

        HelperDatabaseBackup.backup(root, LocalDate.of(2026, 10, 27));
        Files.writeString(root.toPath().resolve("sqlite"), "second");
        HelperDatabaseBackup.backup(root, LocalDate.of(2026, 10, 27));

        Path day = root.toPath().resolve("backups").resolve("20261027");
        assertEquals("first", Files.readString(day.resolve("sqlite")));
    }

    @Test
    public void skipsCleanInstallAndCreatesNothing() {
        File root = tmp.getRoot();

        HelperDatabaseBackup.backup(root, LocalDate.of(2026, 10, 27));

        assertFalse(new File(root, "backups").exists());
    }
}
