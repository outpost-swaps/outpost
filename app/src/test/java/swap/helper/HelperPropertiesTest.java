package swap.helper;

import org.junit.Assume;
import org.junit.Rule;
import org.junit.Test;
import org.junit.rules.TemporaryFolder;

import java.io.InputStream;
import java.nio.file.FileSystems;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.attribute.PosixFilePermissions;
import java.util.Properties;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertNotEquals;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

public class HelperPropertiesTest {
    @Rule
    public TemporaryFolder tmp = new TemporaryFolder();

    @Test
    public void dojoApiKeyMigratesFromPlaintextToPassphraseCiphertext() throws Exception {
        Properties props = new Properties();
        props.setProperty("cli.dojo.apiKey", "dojo-secret");

        assertTrue(HelperProperties.migrateDojoApiKey(props, "test-passphrase"));
        assertNull(props.getProperty("cli.dojo.apiKey"));
        String encrypted = props.getProperty("cli.dojo.apiKeyEncrypted");
        assertNotNull(encrypted);
        assertNotEquals("dojo-secret", encrypted);
        assertEquals("dojo-secret", HelperProperties.dojoApiKey);

        HelperProperties.dojoApiKey = null;
        assertFalse(HelperProperties.migrateDojoApiKey(props, "test-passphrase"));
        assertEquals("dojo-secret", HelperProperties.dojoApiKey);

        Properties emptyPassphrase = new Properties();
        emptyPassphrase.setProperty("cli.dojo.apiKey", "dojo-secret");
        assertTrue(HelperProperties.migrateDojoApiKey(emptyPassphrase, ""));
        HelperProperties.dojoApiKey = null;
        assertFalse(HelperProperties.migrateDojoApiKey(emptyPassphrase, ""));
        assertEquals("dojo-secret", HelperProperties.dojoApiKey);
    }

    // At first pairing the Dojo key used to be written to disk in plaintext until
    // the next unlock. It is now kept in memory and encrypted when the passphrase
    // is first available.
    @Test
    public void aPairingKeyHeldInMemoryIsEncryptedAtUnlockAndNeverStoredInPlaintext() throws Exception {
        Properties props = new Properties();
        HelperProperties.dojoApiKey = "mem-secret";

        assertTrue(HelperProperties.migrateDojoApiKey(props, "test-passphrase"));

        assertNull(props.getProperty("cli.dojo.apiKey"));
        String encrypted = props.getProperty("cli.dojo.apiKeyEncrypted");
        assertNotNull(encrypted);
        assertNotEquals("mem-secret", encrypted);

        HelperProperties.dojoApiKey = null;
        assertFalse(HelperProperties.migrateDojoApiKey(props, "test-passphrase"));
        assertEquals("mem-secret", HelperProperties.dojoApiKey);
    }

    @Test
    public void nothingIsStoredWhenThereIsNoKeyAnywhere() throws Exception {
        Properties props = new Properties();
        HelperProperties.dojoApiKey = null;

        assertFalse(HelperProperties.migrateDojoApiKey(props, "test-passphrase"));
        assertNull(props.getProperty("cli.dojo.apiKeyEncrypted"));
    }

    // First review #9: setProperty/store did load -> modify -> store with no lock, so two threads
    // changing different keys could overwrite each other (a lost update could drop the seed or
    // the Dojo key written a moment earlier).
    @Test
    public void concurrentUpdatesOfDifferentKeysAreAllKept() throws Exception {
        Path target = tmp.newFile("atomic-swaps.properties").toPath();
        Files.writeString(target, "seed=keep\n");

        int threads = 24;
        int perThread = 6;
        java.util.concurrent.ExecutorService pool = java.util.concurrent.Executors.newFixedThreadPool(threads);
        java.util.concurrent.CountDownLatch go = new java.util.concurrent.CountDownLatch(1);
        java.util.List<java.util.concurrent.Future<?>> futures = new java.util.ArrayList<>();
        for (int t = 0; t < threads; t++) {
            final int thread = t;
            futures.add(pool.submit(() -> {
                go.await();
                for (int i = 0; i < perThread; i++) {
                    final String key = "k" + thread + "_" + i;
                    HelperProperties.update(target, props -> props.setProperty(key, "v"));
                }
                return null;
            }));
        }
        go.countDown();
        for (java.util.concurrent.Future<?> future : futures) {
            future.get();
        }
        pool.shutdown();

        Properties loaded = new Properties();
        try (InputStream input = Files.newInputStream(target)) {
            loaded.load(input);
        }
        assertEquals("keep", loaded.getProperty("seed"));
        assertEquals(threads * perThread + 1, loaded.size());
    }

    @Test
    public void restrictDirectoryToOwnerRemovesGroupAndOtherAccess() throws Exception {
        Assume.assumeTrue(
                FileSystems.getDefault().supportedFileAttributeViews().contains("posix"));

        Path dir = tmp.newFolder("app-dir").toPath();
        Files.setPosixFilePermissions(dir, PosixFilePermissions.fromString("rwxr-xr-x"));

        HelperProperties.restrictDirectoryToOwner(dir);

        assertEquals(
                PosixFilePermissions.fromString("rwx------"), Files.getPosixFilePermissions(dir));
    }

    @Test
    public void storeAtomicallyReplacesFileWithOwnerOnlyPermissions() throws Exception {
        Assume.assumeTrue(
                FileSystems.getDefault().supportedFileAttributeViews().contains("posix"));

        Path target = tmp.newFile("atomic-swaps.properties").toPath();
        Files.writeString(target, "old=value");
        Files.setPosixFilePermissions(target, PosixFilePermissions.fromString("rw-r--r--"));

        Properties props = new Properties();
        props.setProperty("cli.dojo.apiKey", "secret");

        HelperProperties.storeAtomically(props, "test", target);

        Properties loaded = new Properties();
        try (InputStream input = Files.newInputStream(target)) {
            loaded.load(input);
        }
        assertEquals("secret", loaded.getProperty("cli.dojo.apiKey"));
        assertEquals(
                PosixFilePermissions.fromString("rw-------"),
                Files.getPosixFilePermissions(target));
        assertEquals(1, tmp.getRoot().list().length);
    }

    @Test
    public void aDifferentDojoIsSavedWithItsKeyEncryptedUnderThePassphrase() throws Exception {
        Properties props = new Properties();
        props.setProperty("cli.dojo.apiKey", "old-plaintext-key");
        props.setProperty("cli.dojo.url", "http://old.onion/v2");

        HelperProperties.applyDojo(props, "http://new.onion/v2", "brand-new-key", "my passphrase");

        assertEquals("http://new.onion/v2", props.getProperty("cli.dojo.url"));
        assertEquals("true", props.getProperty("cli.dojo.enabled"));
        assertNull("no plaintext key may remain", props.getProperty("cli.dojo.apiKey"));
        String encrypted = props.getProperty("cli.dojo.apiKeyEncrypted");
        assertFalse(encrypted.contains("brand-new-key"));
        assertEquals("brand-new-key", com.samourai.wallet.crypto.AESUtil.decrypt(encrypted, new com.samourai.wallet.util.CharSequenceX("my passphrase")));
    }
}
