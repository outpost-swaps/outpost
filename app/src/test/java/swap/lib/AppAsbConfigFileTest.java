package swap.lib;

import static org.junit.Assert.assertEquals;

import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.attribute.PosixFilePermissions;
import org.junit.Assume;
import org.junit.Rule;
import org.junit.Test;
import org.junit.rules.TemporaryFolder;

/** The ASB config holds the wallet RPC login in its URL, so only the owner may read it. */
public class AppAsbConfigFileTest {
    @Rule public TemporaryFolder tmp = new TemporaryFolder();

    @Test
    public void theConfigFileIsOwnerOnlyFromTheMomentItHasContent() throws Exception {
        Assume.assumeTrue(
                java.nio.file.FileSystems.getDefault().supportedFileAttributeViews().contains("posix"));
        Path file = tmp.getRoot().toPath().resolve("config.toml");

        AppAsb.writeOwnerOnly(file, "[monero]\nwallet_rpc_url = \"http://u:p@127.0.0.1:1/json_rpc\"\n");

        assertEquals(PosixFilePermissions.fromString("rw-------"), Files.getPosixFilePermissions(file));
        assertEquals("[monero]\nwallet_rpc_url = \"http://u:p@127.0.0.1:1/json_rpc\"\n", Files.readString(file));
    }

    @Test
    public void anExistingWorldReadableFileIsReplacedNotReused() throws Exception {
        Assume.assumeTrue(
                java.nio.file.FileSystems.getDefault().supportedFileAttributeViews().contains("posix"));
        Path file = tmp.newFile("config.toml").toPath();
        Files.setPosixFilePermissions(file, PosixFilePermissions.fromString("rw-r--r--"));

        AppAsb.writeOwnerOnly(file, "new\n");

        assertEquals(PosixFilePermissions.fromString("rw-------"), Files.getPosixFilePermissions(file));
        assertEquals("new\n", Files.readString(file));
    }
}
