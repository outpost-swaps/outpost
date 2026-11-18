package swap.process;

import java.io.IOException;
import java.util.concurrent.TimeUnit;

/**
 * Graceful process termination with a bounded fallback to force-kill: SIGTERM
 * (or WM_CLOSE on Windows) first, a bounded wait, then force-kill. Force-kill
 * of a running monero-wallet-rpc or ASB can leave wallet state unflushed.
 */
public final class ProcessGraceful {
    private static final long WAIT_MS = 10_000;

    private ProcessGraceful() {
    }

    public static boolean shutdown(Process process) {
        if (process == null) {
            return false;
        }
        process.destroy();
        try {
            if (!process.waitFor(WAIT_MS, TimeUnit.MILLISECONDS)) {
                process.destroyForcibly();
                process.waitFor(WAIT_MS, TimeUnit.MILLISECONDS);
            }
            return true;
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            process.destroyForcibly();
            return false;
        }
    }

    /** A PID is a plain number: nothing else may reach the command line. */
    static boolean isValidPid(String pid) {
        return pid != null && pid.matches("[0-9]{1,10}");
    }

    /** Whether {@code commandLine} runs the program {@code expected} (a whole program name). */
    static boolean commandMatches(String commandLine, String expected) {
        if (commandLine == null || commandLine.isBlank() || expected == null || expected.isBlank()) {
            return false;
        }
        return java.util.regex.Pattern
                .compile("(^|[/\\\\\\s])" + java.util.regex.Pattern.quote(expected) + "(\\.exe)?(\\s|$)")
                .matcher(commandLine)
                .find();
    }

    /** PIDs in `ps -axo pid=,command=` output whose command is exactly {@code executablePath}. */
    static java.util.List<String> parsePidsRunning(String psOutput, String executablePath) {
        java.util.List<String> pids = new java.util.ArrayList<>();
        for (String line : psOutput.split("\\R")) {
            String trimmed = line.trim();
            int space = trimmed.indexOf(' ');
            if (space < 0) {
                continue;
            }
            String pid = trimmed.substring(0, space);
            String command = trimmed.substring(space + 1).trim();
            if (isValidPid(pid)
                    && (command.equals(executablePath) || command.startsWith(executablePath + " "))) {
                pids.add(pid);
            }
        }
        return pids;
    }

    /**
     * Stops every process still running {@code executablePath}, for helper programs an earlier
     * run left behind (a crash, a kill). Each one is checked again by {@link #shutdownPid}
     * before it is signalled. Does nothing on Windows, where the process list has no paths.
     *
     * @return how many were stopped
     */
    public static int stopStrays(String executablePath, String expectedProgram) {
        if (isWindows()) {
            return 0;
        }
        try {
            Process list = new ProcessBuilder("ps", "-axo", "pid=,command=").start();
            String output = new String(list.getInputStream().readAllBytes(), java.nio.charset.StandardCharsets.UTF_8);
            list.waitFor(5, TimeUnit.SECONDS);
            int stopped = 0;
            for (String pid : parsePidsRunning(output, executablePath)) {
                if (shutdownPid(pid, expectedProgram)) {
                    stopped++;
                }
            }
            return stopped;
        } catch (IOException | InterruptedException e) {
            if (e instanceof InterruptedException) {
                Thread.currentThread().interrupt();
            }
            return 0;
        }
    }

    /** The command line of a running PID, or {@code null} when no such process exists. */
    private static String commandLineOf(String pid) throws IOException, InterruptedException {
        Process probe = isWindows()
                ? new ProcessBuilder("tasklist", "/FI", "PID eq " + pid, "/FO", "CSV", "/NH").start()
                : new ProcessBuilder("ps", "-p", pid, "-o", "command=").start();
        String output = new String(probe.getInputStream().readAllBytes(), java.nio.charset.StandardCharsets.UTF_8).trim();
        probe.waitFor(5, TimeUnit.SECONDS);
        if (output.isEmpty() || output.startsWith("INFO:")) {
            return null;
        }
        return output;
    }

    /**
     * Graceful shutdown for a PID from a previous app run (not owned by this
     * JVM, so there is no Process handle): SIGTERM, bounded liveness polling,
     * then SIGKILL.
     *
     * <p>The PID is only a number saved by an earlier run, so by now it can belong to any
     * program (after a reboot, for instance). The process is stopped only if its command line
     * still shows {@code expectedProgram}; otherwise it is left alone.
     */
    public static boolean shutdownPid(String pid, String expectedProgram) {
        if (!isValidPid(pid)) {
            System.out.println("Not stopping a process: " + pid + " is not a process id");
            return false;
        }
        try {
            String commandLine = commandLineOf(pid);
            if (commandLine == null) {
                return true; // already gone
            }
            if (!commandMatches(commandLine, expectedProgram)) {
                System.out.println("PID " + pid + " is no longer " + expectedProgram + "; leaving it running");
                return false;
            }
            if (isWindows()) {
                Runtime.getRuntime().exec(new String[] {"taskkill", "/pid", pid});
                if (!aliveAfterGrace(pid)) {
                    return true;
                }
                Runtime.getRuntime().exec(new String[] {"taskkill", "/f", "/pid", pid});
                return true;
            }

            Runtime.getRuntime().exec(new String[] {"kill", "-TERM", pid});
            long deadline = System.currentTimeMillis() + WAIT_MS;
            while (System.currentTimeMillis() < deadline && pidAlive(pid)) {
                TimeUnit.MILLISECONDS.sleep(250);
            }
            if (pidAlive(pid)) {
                Runtime.getRuntime().exec(new String[] {"kill", "-9", pid});
            }
            return true;
        } catch (IOException | InterruptedException e) {
            if (e instanceof InterruptedException) {
                Thread.currentThread().interrupt();
            }
            return false;
        }
    }

    private static boolean isWindows() {
        return System.getProperty("os.name").toLowerCase().contains("windows");
    }

    private static boolean pidAlive(String pid) throws IOException {
        Process probe = Runtime.getRuntime().exec(new String[] {"kill", "-0", pid});
        try {
            return probe.waitFor(2, TimeUnit.SECONDS) && probe.exitValue() == 0;
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return false;
        }
    }

    private static boolean aliveAfterGrace(String pid) throws IOException, InterruptedException {
        long deadline = System.currentTimeMillis() + WAIT_MS;
        while (System.currentTimeMillis() < deadline) {
            Process probe = Runtime.getRuntime().exec(new String[] {"tasklist", "/FI", "PID eq " + pid});
            probe.waitFor(2, TimeUnit.SECONDS);
            // tasklist prints the image name row when the process exists
            if (probe.exitValue() != 0) {
                return false;
            }
            TimeUnit.MILLISECONDS.sleep(250);
        }
        return true;
    }
}
