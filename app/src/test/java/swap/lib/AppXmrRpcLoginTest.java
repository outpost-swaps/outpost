package swap.lib;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.attribute.PosixFilePermissions;
import java.util.ArrayList;
import org.bitcoinj.params.TestNet3Params;
import org.junit.Assume;
import org.junit.Rule;
import org.junit.Test;
import org.junit.rules.TemporaryFolder;

/**
 * monero-wallet-rpc used to be started with `--rpc-login user:password`, which every local user
 * can read with `ps`; the ASB's wallet RPC is the maker's hot wallet.
 */
public class AppXmrRpcLoginTest {
    private static final String PASSWORD = "s3cret-pass-0123456789abcdef";

    @Rule public TemporaryFolder tmp = new TemporaryFolder();

    private ArrayList<String> command(String directory, int port) {
        return AppXmrRpc.buildCommand(
                TestNet3Params.get(), directory, "http://127.0.0.1:9", 0, true, "http://127.0.0.1:9",
                port, "user1", PASSWORD);
    }

    @Test
    public void theLoginIsNotInTheCommandLine() throws Exception {
        ArrayList<String> cmd = command(tmp.getRoot().getAbsolutePath(), 40123);

        assertFalse(cmd.contains("--rpc-login"));
        assertFalse(String.join(" ", cmd).contains(PASSWORD));
        assertTrue(cmd.contains("--config-file"));
    }

    @Test
    public void theLoginIsInAFileOnlyTheOwnerCanRead() throws Exception {
        ArrayList<String> cmd = command(tmp.getRoot().getAbsolutePath(), 40124);
        Path file = Path.of(cmd.get(cmd.indexOf("--config-file") + 1));

        assertEquals("rpc-login=user1:" + PASSWORD + "\n", Files.readString(file));
        Assume.assumeTrue(
                java.nio.file.FileSystems.getDefault().supportedFileAttributeViews().contains("posix"));
        assertEquals(PosixFilePermissions.fromString("rw-------"), Files.getPosixFilePermissions(file));
    }

    @Test
    public void aRestartRewritesTheFileForTheSamePort() throws Exception {
        String dir = tmp.getRoot().getAbsolutePath();
        ArrayList<String> first = command(dir, 40125);
        ArrayList<String> second = command(dir, 40125);

        assertEquals(first.get(first.indexOf("--config-file") + 1), second.get(second.indexOf("--config-file") + 1));
    }

    @Test
    public void loginFilesOfEarlierRunsAreRemoved() throws Exception {
        Path stale = tmp.newFile("wallet-rpc-login-1111.conf").toPath();
        Path unrelated = tmp.newFile("keep-me.txt").toPath();

        AppXmrRpc.deleteStaleLoginFiles(tmp.getRoot().getAbsolutePath());

        assertFalse(Files.exists(stale));
        assertTrue(Files.exists(unrelated));
    }
}
