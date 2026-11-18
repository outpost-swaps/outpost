package swap.helper;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.File;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.nio.file.attribute.PosixFilePermissions;
import java.time.LocalDate;
import java.time.format.DateTimeFormatter;
import java.util.ArrayList;
import java.util.List;

/**
 * Best-effort daily snapshot of the swap state, taken at app startup before the
 * database is opened. SQLite WAL/SHM companions are copied with the main file.
 * Backups live under {@code backups/YYYYMMDD/} and are finalized with a
 * {@code .complete} marker so a partial copy is retried on the next launch.
 *
 * Seeds are deliberately not copied; this only protects swap state and the
 * swap metadata file.
 */
public final class HelperDatabaseBackup {
    private static final Logger log = LoggerFactory.getLogger(HelperDatabaseBackup.class);
    private static final DateTimeFormatter DAY = DateTimeFormatter.ofPattern("yyyyMMdd");

    private HelperDatabaseBackup() {
    }

    public static void backupDaily(File root) {
        backup(root, LocalDate.now());
    }

    static void backup(File root, LocalDate day) {
        File dayDir = new File(new File(root, "backups"), day.format(DAY));
        File complete = new File(dayDir, ".complete");
        if (complete.isFile()) {
            return;
        }

        List<File> sources = new ArrayList<>();
        sources.add(new File(root, "sqlite"));
        sources.add(new File(root, "swaps.json"));
        sources.add(new File(root, "asb/testnet/sqlite"));
        sources.add(new File(root, "asb/mainnet/sqlite"));

        boolean copied = false;
        for (File source : sources) {
            if (!source.isFile()) {
                continue;
            }
            copied |= copyWithSqliteCompanions(source, root, dayDir);
        }

        if (!copied) {
            return;
        }

        try {
            Files.createDirectories(dayDir.toPath());
            restrictPermissions(dayDir.toPath(), "rwx------");
            Files.writeString(complete.toPath(), "ok\n");
            restrictPermissions(complete.toPath(), "rw-------");
        } catch (IOException e) {
            log.warn("Could not finalize database backup in {}: {}", dayDir, e.getMessage());
        }
    }

    private static boolean copyWithSqliteCompanions(File source, File root, File dayDir) {
        boolean copied = false;
        for (String suffix : new String[] { "", "-wal", "-shm" }) {
            File part = new File(source.getPath() + suffix);
            if (!part.isFile()) {
                continue;
            }

            Path relative = root.toPath().relativize(part.toPath());
            Path target = dayDir.toPath().resolve(relative);
            try {
                Files.createDirectories(target.getParent());
                Files.copy(
                        part.toPath(),
                        target,
                        StandardCopyOption.REPLACE_EXISTING,
                        StandardCopyOption.COPY_ATTRIBUTES
                );
                restrictPermissions(target.getParent(), "rwx------");
                restrictPermissions(target, "rw-------");
                copied = true;
            } catch (IOException e) {
                log.warn("Could not back up {} to {}: {}", part, target, e.getMessage());
            }
        }
        return copied;
    }

    private static void restrictPermissions(Path path, String permissions) {
        try {
            Files.setPosixFilePermissions(path, PosixFilePermissions.fromString(permissions));
        } catch (UnsupportedOperationException | IOException e) {
            log.debug("Could not restrict permissions on {}: {}", path, e.getMessage());
        }
    }
}
