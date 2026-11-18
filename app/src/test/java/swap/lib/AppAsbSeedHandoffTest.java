package swap.lib;

import org.bitcoinj.params.TestNet3Params;
import org.junit.After;
import org.junit.Before;
import org.junit.Test;
import swap.gui.GUISwap;
import swap.model.request.StartAsbRequest;
import swap.process.ProcessAsb;

import java.io.IOException;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.concurrent.TimeUnit;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.assertThrows;

public class AppAsbSeedHandoffTest {
    private Path swapRoot;

    @Before
    public void setUp() throws Exception {
        swapRoot = Files.createTempDirectory("swaps-asb-test");
        // Route AppSwap.getSwapRootDir() to the temp dir for this test.
        Field rootField = Class.forName("swap.lib.AppSwap").getDeclaredField("swapRootDir");
        rootField.setAccessible(true);
        rootField.set(null, swapRoot.toString());
    }

    @After
    public void tearDown() throws Exception {
        Field rootField = Class.forName("swap.lib.AppSwap").getDeclaredField("swapRootDir");
        rootField.setAccessible(true);
        rootField.set(null, null);
    }

    @Test
    public void seedPayloadIsWrittenAsBase64AsciiWithoutPemWrapping() {
        String seed = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+Pw==";

        assertArrayEquals(seed.getBytes(StandardCharsets.US_ASCII), AppAsb.seedStdinPayload(seed));
    }

    @Test
    public void missingSeedPreventsStartingAsb() {
        assertThrows(IllegalStateException.class, () -> AppAsb.seedStdinPayload(""));
        assertThrows(IllegalStateException.class, () -> AppAsb.seedStdinPayload(null));
    }

    @Test
    public void seedIsWrittenThroughAndClosesARealChildProcessPipe() throws Exception {
        String java = System.getProperty("java.home") + "/bin/java";
        String childClass = AppAsbSeedHandoffTest.class.getName() + "$EchoStdinChild";
        Process child = new ProcessBuilder(java, "-cp", System.getProperty("java.class.path"), childClass)
                .redirectErrorStream(true)
                .start();
        byte[] payload = AppAsb.seedStdinPayload("test-seed-data");

        AppAsb.writeSeedToProcess(child, payload);

        assertTrue("child stdin should close and let the process exit", child.waitFor(10, TimeUnit.SECONDS));
        assertEquals(0, child.exitValue());
        assertArrayEquals(payload, child.getInputStream().readAllBytes());
    }

    @Test
    public void failedLaunchClearsTheRunningFlag() throws Exception {
        // start() marks the ASB running before startAsb() runs, so a failed launch must
        // clear the flag instead of leaving a "running" ASB that does not exist.
        AppAsb appAsb = new AppAsb();
        appAsb.setRunningAsb(true);
        assertTrue(appAsb.getRunningAsb());

        // In unit tests the launch cannot complete (no bundled ASB binary/network setup),
        // which drives startAsb() through its failure path before any process spawns.
        startAsb(appAsb, new StartAsbRequest(
                "electrum.invalid:50001", List.of(), "127.0.0.1:9050", 9050, "http://127.0.0.1:18083",
                "http://127.0.0.1:18081", List.of(), false, 0.001f, 0.1f, 0.01f, null, TestNet3Params.get(), "dummy.onion"));

        assertFalse("a failed ASB launch must clear the running flag", appAsb.getRunningAsb());
    }

    @Test
    public void earlyAsbExitClearsTheRunningFlag() throws Exception {
        GUISwap.appAsb.setRunningAsb(true);
        assertTrue(GUISwap.appAsb.getRunningAsb());

        String java = System.getProperty("java.home") + "/bin/java";
        Process child = new ProcessBuilder(java, "-cp", System.getProperty("java.class.path"),
                AppAsbSeedHandoffTest.class.getName() + "$ExitImmediatelyChild")
                .start();
        // Let the child die before the reader task starts: readLine on the closed
        // stream then returns null immediately with the process already dead, so the
        // early-exit handling runs deterministically instead of racing child exit.
        assertTrue("child should exit promptly", child.waitFor(10, TimeUnit.SECONDS));
        assertEquals(3, child.exitValue());
        // The record constructor starts the stderr log-reader task; the dead child
        // exercises the early-exit handling.
        new ProcessAsb(child);

        long deadline = System.currentTimeMillis() + 10_000;
        while (GUISwap.appAsb.getRunningAsb() && System.currentTimeMillis() < deadline) {
            Thread.sleep(50);
        }
        assertFalse("an early ASB exit must clear the running flag", GUISwap.appAsb.getRunningAsb());
    }

    private static void startAsb(AppAsb appAsb, StartAsbRequest request) throws Exception {
        Method startAsb = AppAsb.class.getDeclaredMethod("startAsb", StartAsbRequest.class);
        startAsb.setAccessible(true);
        startAsb.invoke(appAsb, request);
    }

    public static final class EchoStdinChild {
        public static void main(String[] args) throws IOException {
            System.in.transferTo(System.out);
            System.out.flush();
        }
    }

    public static final class ExitImmediatelyChild {
        public static void main(String[] args) {
            System.exit(3);
        }
    }
}
