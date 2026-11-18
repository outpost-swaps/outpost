package swap.process;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.util.concurrent.TimeUnit;
import org.junit.Assume;
import org.junit.Test;

public class ProcessGracefulTest {

    @Test
    public void onlyPlainNumbersAreAcceptedAsPids() {
        assertTrue(ProcessGraceful.isValidPid("4242"));
        assertFalse(ProcessGraceful.isValidPid(""));
        assertFalse(ProcessGraceful.isValidPid(null));
        assertFalse(ProcessGraceful.isValidPid("12 -9 1"));
        assertFalse(ProcessGraceful.isValidPid("-1"));
        assertFalse(ProcessGraceful.isValidPid("123456789012"));
    }

    @Test
    public void aCommandLineMatchesOnlyWhenItRunsTheExpectedProgram() {
        String walletRpc = "/Users/me/Library/Application Support/outpost-swaps/monero/monero-wallet-rpc --daemon-address http://127.0.0.1:1";
        assertTrue(ProcessGraceful.commandMatches(walletRpc, "monero-wallet-rpc"));
        assertTrue(ProcessGraceful.commandMatches("C:\\x\\monero-wallet-rpc.exe --stagenet", "monero-wallet-rpc"));
        assertTrue(ProcessGraceful.commandMatches("/x/asb/bin/asb --testnet --json", "asb"));
        assertFalse(ProcessGraceful.commandMatches("/Applications/Firefox.app/Contents/MacOS/firefox", "monero-wallet-rpc"));
        // the name must be a whole program name, not a fragment of another one
        assertFalse(ProcessGraceful.commandMatches("/usr/bin/basbackup --x", "asb"));
        assertFalse(ProcessGraceful.commandMatches("", "asb"));
        assertFalse(ProcessGraceful.commandMatches(null, "asb"));
    }

    // PID reuse: a stored PID from an earlier run can belong to any program by now. Before, the
    // app sent it SIGTERM and then SIGKILL without looking.
    @Test
    public void aProcessThatIsNotTheExpectedProgramIsLeftAlone() throws Exception {
        Assume.assumeFalse(System.getProperty("os.name").toLowerCase().contains("windows"));
        Process unrelated = new ProcessBuilder("sleep", "60").start();
        try {
            ProcessGraceful.shutdownPid(String.valueOf(unrelated.pid()), "monero-wallet-rpc");
            assertTrue("an unrelated process must survive", unrelated.isAlive());
        } finally {
            unrelated.destroyForcibly();
        }
    }

    @Test
    public void theExpectedProgramIsStopped() throws Exception {
        Assume.assumeFalse(System.getProperty("os.name").toLowerCase().contains("windows"));
        Process target = new ProcessBuilder("sleep", "60").start();
        try {
            assertTrue(ProcessGraceful.shutdownPid(String.valueOf(target.pid()), "sleep"));
            assertTrue(target.waitFor(15, TimeUnit.SECONDS));
        } finally {
            target.destroyForcibly();
        }
    }

    @Test
    public void aProcessThatIsAlreadyGoneIsNotAnError() throws Exception {
        Assume.assumeFalse(System.getProperty("os.name").toLowerCase().contains("windows"));
        Process done = new ProcessBuilder("true").start();
        done.waitFor();
        assertEquals(true, ProcessGraceful.shutdownPid(String.valueOf(done.pid()), "true"));
    }

    // Forwarders left by an earlier run (a crash, a kill) keep running with the SOCKS port of a
    // Tor that no longer exists. They are found by the path of the program they run.
    @Test
    public void psOutputIsReducedToTheProcessesRunningThatProgram() {
        String ps = ""
                + "  101 /Users/me/Library/Application Support/outpost-swaps/monero/daemon_pool_proxy --listen 127.0.0.1:1\n"
                + "  102 /Users/me/Library/Application Support/outpost-swaps/monero/daemon_pool_proxy_electrum --listen 127.0.0.1:2\n"
                + "  103 /usr/bin/vim /Users/me/Library/Application Support/outpost-swaps/monero/daemon_pool_proxy\n"
                + "  104 /Users/me/Library/Application Support/outpost-swaps/monero/daemon_pool_proxy\n"
                + "garbage line\n";
        String forwarder = "/Users/me/Library/Application Support/outpost-swaps/monero/daemon_pool_proxy";

        assertEquals(java.util.List.of("101", "104"), ProcessGraceful.parsePidsRunning(ps, forwarder));
    }

    @Test
    public void strayProcessesOfOurProgramAreStoppedAndOthersAreNot() throws Exception {
        Assume.assumeFalse(System.getProperty("os.name").toLowerCase().contains("windows"));
        java.nio.file.Path dir = java.nio.file.Files.createTempDirectory("forwarder-test");
        java.nio.file.Path fake = dir.resolve("fake_forwarder");
        // A script that runs `sleep` under its own path as argv[0], so `ps` shows the path the
        // way it shows a real forwarder. (A copied system binary is killed by code signing.)
        java.nio.file.Files.writeString(fake, "#!/bin/bash\nexec -a \"$0\" /bin/sleep \"$1\"\n");
        Assume.assumeTrue(fake.toFile().setExecutable(true));

        Process first;
        Process second;
        try {
            first = new ProcessBuilder(fake.toString(), "60").start();
            second = new ProcessBuilder(fake.toString(), "61").start();
        } catch (java.io.IOException e) {
            Assume.assumeNoException("cannot run a copied system binary here", e);
            return;
        }
        Process other = new ProcessBuilder("sleep", "62").start();
        try {
            // Each script is `bash` until its `exec` finishes, and only then does `ps` show the
            // path as the command. On a busy machine that can take longer than a fixed sleep.
            waitUntilListed(fake.toString(), 2);
            assertEquals(2, ProcessGraceful.stopStrays(fake.toString(), "fake_forwarder"));
            assertTrue(first.waitFor(15, TimeUnit.SECONDS));
            assertTrue(second.waitFor(15, TimeUnit.SECONDS));
            assertTrue("an unrelated process must survive", other.isAlive());
        } finally {
            first.destroyForcibly();
            second.destroyForcibly();
            other.destroyForcibly();
        }
    }

    /** Waits (up to 15 s) until {@code ps} lists {@code count} processes running {@code path}. */
    private static void waitUntilListed(String path, int count) throws Exception {
        long deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(15);
        while (System.nanoTime() < deadline) {
            Process list = new ProcessBuilder("ps", "-axo", "pid=,command=").start();
            String output = new String(list.getInputStream().readAllBytes(), java.nio.charset.StandardCharsets.UTF_8);
            list.waitFor(5, TimeUnit.SECONDS);
            if (ProcessGraceful.parsePidsRunning(output, path).size() >= count) {
                return;
            }
            Thread.sleep(20);
        }
        throw new AssertionError(count + " processes running " + path + " did not show up in ps");
    }
}
