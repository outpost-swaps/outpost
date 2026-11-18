package swap.wallet;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import java.nio.file.Files;
import java.nio.file.Paths;
import org.junit.Test;

/** A 10-15 s Dojo blip over Tor stays out of the log; an outage of a minute or more is logged once. */
public class DojoOutageNoticeTest {
    private static final long REFRESH = 15_000; // SwapsWallet refreshes balances every 15 s

    @Test
    public void aBlipThatRecoversWithinTheGraceIsNotLogged() {
        DojoOutageNotice notice = new DojoOutageNotice();
        assertFalse(notice.failed(0));
        assertFalse(notice.failed(REFRESH)); // still failing 15 s later
        assertFalse("no warning was shown, so no recovery line", notice.succeeded());
    }

    @Test
    public void anOutageOfAMinuteIsLoggedOnceThenItsRecovery() {
        DojoOutageNotice notice = new DojoOutageNotice();
        assertFalse(notice.failed(0));
        assertFalse(notice.failed(DojoOutageNotice.GRACE_MILLIS - 1));
        assertTrue(notice.failed(DojoOutageNotice.GRACE_MILLIS));
        assertFalse("one warning per outage", notice.failed(DojoOutageNotice.GRACE_MILLIS + REFRESH));
        assertTrue(notice.succeeded());
        assertFalse(notice.succeeded());
    }

    @Test
    public void eachOutageStartsItsOwnClock() {
        DojoOutageNotice notice = new DojoOutageNotice();
        notice.failed(0);
        notice.succeeded();
        // a second blip much later is measured from its own start, not the first failure
        assertFalse(notice.failed(10 * DojoOutageNotice.GRACE_MILLIS));
        assertTrue(notice.failed(11 * DojoOutageNotice.GRACE_MILLIS));
    }

    /** Source check: the balance refresh uses the notice for both lines (SwapsWallet needs Dojo). */
    @Test
    public void theBalanceRefreshLogsThroughTheNotice() throws Exception {
        String source = new String(Files.readAllBytes(Paths.get("src/main/java/swap/wallet/SwapsWallet.java")));
        String unreachable = source.substring(source.indexOf("private void markDojoUnreachable("),
                source.indexOf("private boolean isStoppedHttpClient("));
        assertTrue(unreachable.contains("if (dojoOutage.failed(System.currentTimeMillis()))"));
        assertFalse("a single failure must not log", unreachable.contains("indexSyncFailureReported.compareAndSet"));
        String reachable = source.substring(source.indexOf("private void markDojoReachable("),
                source.indexOf("private void markDojoUnreachable("));
        assertTrue(reachable.contains("dojoOutage.succeeded()"));
    }

    /** The log view gets a right-click menu (RichTextFX has none of its own). */
    @Test
    public void theLogHasACopyMenu() throws Exception {
        String source = new String(Files.readAllBytes(Paths.get("src/main/java/swap/gui/controller/MainController.java")));
        assertTrue(source.contains("installLogContextMenu();"));
        String menu = source.substring(source.indexOf("private void installLogContextMenu()"));
        menu = menu.substring(0, menu.indexOf("\n    }\n"));
        assertTrue(menu.contains("new javafx.scene.control.MenuItem(\"Copy\")"));
        assertTrue(menu.contains("new javafx.scene.control.MenuItem(\"Copy all\")"));
        assertTrue(menu.contains("logs.setContextMenu(menu)"));
    }
}
