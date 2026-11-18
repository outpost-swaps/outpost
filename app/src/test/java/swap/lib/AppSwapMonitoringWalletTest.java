package swap.lib;

import org.junit.Rule;
import org.junit.Test;
import org.junit.rules.TemporaryFolder;

import java.io.File;
import java.io.IOException;
import java.nio.file.Files;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

/**
 * Regression coverage for the monitoring-wallet half-state that blocked swap
 * resume: a cleanup racing monero-wallet-rpc's shutdown save left the cache
 * file without its .keys sibling. open_wallet then fails with file_not_found
 * and create_wallet fails with file_exists.
 *
 * The sweep must remove only incomplete pairs, after the old rpc is gone, so a
 * live/resumable wallet is never deleted.
 */
public class AppSwapMonitoringWalletTest {

    @Rule
    public TemporaryFolder folder = new TemporaryFolder();

    private static final String STALE = "324dedd9-9c73-4c01-9bc7-5979e5690547-monitoring-wallet";

    @Test
    public void removesCacheLeftWithoutKeys() throws IOException {
        File cache = createFile(STALE);

        int deleted = AppSwap.deleteStaleMonitoringWalletFiles(folder.getRoot());

        assertEquals(1, deleted);
        assertFalse(cache.exists());
        assertFalse(keysOf(STALE).exists());
    }

    @Test
    public void removesKeysLeftWithoutCache() throws IOException {
        File keys = createFile(STALE + ".keys");

        int deleted = AppSwap.deleteStaleMonitoringWalletFiles(folder.getRoot());

        assertEquals(1, deleted);
        assertFalse(keys.exists());
        assertFalse(new File(folder.getRoot(), STALE).exists());
    }

    @Test
    public void keepsACompletePair() throws IOException {
        File cache = createFile(STALE);
        File keys = createFile(STALE + ".keys");

        int deleted = AppSwap.deleteStaleMonitoringWalletFiles(folder.getRoot());

        assertEquals(0, deleted);
        assertTrue(cache.exists());
        assertTrue(keys.exists());
    }

    @Test
    public void keepsUnrelatedWalletFiles() throws IOException {
        File asbCache = createFile("asb-wallet");
        File asbKeys = createFile("asb-wallet.keys");
        File swapToolCache = createFile("swap-tool-blockchain-monitoring-wallet");

        int deleted = AppSwap.deleteStaleMonitoringWalletFiles(folder.getRoot());

        assertEquals(0, deleted);
        assertTrue(asbCache.exists());
        assertTrue(asbKeys.exists());
        assertTrue(swapToolCache.exists());
    }

    @Test
    public void removesOnlyTheIncompleteSwapWhenSeveralExist() throws IOException {
        File staleCache = createFile(STALE);
        File completeCache = createFile("aaaaaaaa-0000-0000-0000-000000000000-monitoring-wallet");
        File completeKeys = createFile("aaaaaaaa-0000-0000-0000-000000000000-monitoring-wallet.keys");

        int deleted = AppSwap.deleteStaleMonitoringWalletFiles(folder.getRoot());

        assertEquals(1, deleted);
        assertFalse(staleCache.exists());
        assertTrue(completeCache.exists());
        assertTrue(completeKeys.exists());
    }

    @Test
    public void isIdempotent() throws IOException {
        createFile(STALE);

        AppSwap.deleteStaleMonitoringWalletFiles(folder.getRoot());
        int deleted = AppSwap.deleteStaleMonitoringWalletFiles(folder.getRoot());

        assertEquals(0, deleted);
    }

    private File createFile(String name) throws IOException {
        File file = new File(folder.getRoot(), name);
        Files.write(file.toPath(), new byte[]{1});
        return file;
    }

    private File keysOf(String base) {
        return new File(folder.getRoot(), base + ".keys");
    }
}
