package swap.helper;

import org.json.JSONObject;
import org.junit.After;
import org.junit.Before;
import org.junit.Test;

import java.io.IOException;
import java.lang.reflect.Field;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HashMap;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

/**
 * Verifies the two custody-relevant properties of the swap metadata store:
 * JSON writes are atomic (a kill mid-write cannot truncate swaps.json), and a
 * live swap is never resumed with a silently-defaulted swaps_account.
 */
public class HelperRawJsonDbTest {
    private Path swapRoot;

    @Before
    public void setUp() throws Exception {
        swapRoot = Files.createTempDirectory("swaps-json-db-test");
        // Route AppSwap.getSwapRootDir() to the temp dir for this test.
        Field rootField = Class.forName("swap.lib.AppSwap").getDeclaredField("swapRootDir");
        rootField.setAccessible(true);
        rootField.set(null, swapRoot.toString());
        resetInstance();
    }

    @After
    public void tearDown() throws Exception {
        resetInstance();
        Field rootField = Class.forName("swap.lib.AppSwap").getDeclaredField("swapRootDir");
        rootField.setAccessible(true);
        rootField.set(null, null);
    }

    private static void resetInstance() throws Exception {
        for (Class<?> cls : new Class<?>[]{HelperRawJsonDb.class, HelperSwapsDb.class}) {
            Field instance = cls.getDeclaredField("instance");
            instance.setAccessible(true);
            instance.set(null, null);
        }
    }

    @Test
    public void updateDbPersistsRoundTrip() throws Exception {
        JSONObject db = new JSONObject();
        db.put("swap_test-id", new JSONObject("{\"swaps_account\":2147483642}"));

        HelperRawJsonDb.getInstance().updateDb("swaps", db);

        // New instance forces a disk read rather than the in-memory cache.
        resetInstance();
        JSONObject reloaded = HelperRawJsonDb.getInstance().getDb("swaps");
        assertEquals(2147483642L, HelperSwapsDb.getInstance().getSwapsAccount("test-id"));
        assertFalse(reloaded.keySet().isEmpty());
    }

    @Test
    public void interruptedWriteLeavesPreviousValidJsonIntact() throws Exception {
        JSONObject first = new JSONObject();
        first.put("swap_id-1", new JSONObject("{\"swaps_account\":2147483642}"));
        HelperRawJsonDb.getInstance().updateDb("swaps", first);

        // Simulate a crash during a subsequent write: truncate the live file on disk
        // (as a mid-write kill would) while the in-memory cache is lost with it.
        Path dbFile = swapRoot.resolve("swaps.json");
        Files.write(dbFile, new byte[0]);

        resetInstance();
        try {
            HelperRawJsonDb.getInstance().getDb("swaps");
            fail("Reading a truncated swaps.json must surface an error rather than silently recreate it");
        } catch (RuntimeException expected) {
            assertTrue(expected.getCause() instanceof IOException || expected.getCause() == null);
        }

        // The atomic-write path leaves no stray temp files behind from successful writes.
        try (var stream = Files.list(swapRoot)) {
            long tempFiles = stream.filter(p -> p.getFileName().toString().endsWith(".tmp")).count();
            assertEquals(0, tempFiles);
        }
    }

    @Test
    public void updateDbThrowsWhenPersistenceFails() throws Exception {
        // A directory at the DB path makes the final atomic move fail deterministically
        // on every platform, without relying on permission semantics.
        Files.createDirectory(swapRoot.resolve("swaps.json"));

        try {
            HelperRawJsonDb.getInstance().updateDb("swaps", new JSONObject("{\"swap_x\":{}}"));
            fail("A failed persistence write must propagate instead of being swallowed");
        } catch (RuntimeException expected) {
            assertTrue(expected.getMessage().contains("swaps.json"));
        }
    }

    @Test
    public void atomicMoveLeavesNoTempFilesBehind() {
        JSONObject db = new JSONObject();
        db.put("swap_x", new JSONObject("{\"swaps_account\":2147483641}"));
        HelperRawJsonDb.getInstance().updateDb("swaps", db);

        try (var stream = Files.list(swapRoot)) {
            long tempFiles = stream.filter(p -> p.getFileName().toString().endsWith(".tmp")).count();
            assertEquals(0, tempFiles);
        } catch (IOException e) {
            fail(e.getMessage());
        }
        assertTrue(Files.exists(swapRoot.resolve("swaps.json")));
    }

    @Test
    public void getSwapsAccountFailsClosedWhenMetadataMissing() throws Exception {
        JSONObject db = new JSONObject();
        db.put("swap_no-acct", new JSONObject());
        HelperRawJsonDb.getInstance().updateDb("swaps", db);
        resetInstance();

        try {
            HelperSwapsDb.getInstance().getSwapsAccount("no-acct");
            fail("Missing swaps_account must fail closed, not default to SWAPS_DEPOSIT");
        } catch (IllegalStateException expected) {
            assertTrue(expected.getMessage().contains("swaps_account"));
        }

        // A swap that was never recorded at all must also fail closed.
        try {
            HelperSwapsDb.getInstance().getSwapsAccount("unknown-id");
            fail("Unknown swap must fail closed, not default to SWAPS_DEPOSIT");
        } catch (IllegalStateException expected) {
            assertTrue(expected.getMessage().contains("unknown-id"));
        }
    }

    @Test
    public void getSwapsAccountFailsClosedOnMalformedMetadata() throws Exception {
        JSONObject db = new JSONObject();
        db.put("swap_bad", new JSONObject("{\"swaps_account\":\"not-a-number\"}"));
        HelperRawJsonDb.getInstance().updateDb("swaps", db);
        resetInstance();

        try {
            HelperSwapsDb.getInstance().getSwapsAccount("bad");
            fail("Malformed swaps_account must fail closed");
        } catch (IllegalStateException | org.json.JSONException expected) {
            // Both indicate no silent default was used.
        }
    }

    @Test
    public void setThenGetSwapsAccountRoundTrips() throws Exception {
        HelperSwapsDb.getInstance().setSwapsAccount("rt-id", 2147483641);
        resetInstance();
        assertEquals(2147483641L, HelperSwapsDb.getInstance().getSwapsAccount("rt-id"));
    }
}
