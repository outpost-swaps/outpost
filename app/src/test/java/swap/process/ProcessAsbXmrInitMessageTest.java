package swap.process;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.json.JSONObject;
import org.junit.Test;

public class ProcessAsbXmrInitMessageTest {
    @Test
    public void aNodeThatDidNotAnswerIsNamedAsTheCauseNotADuplicateProcess() {
        String line = ProcessAsb.xmrInitErrorMessage(new JSONObject()
                .put("message", "ASB_ERROR_INITIALIZING_XMR_WALLET")
                .put("error", "JSON-RPC request failed with code -38: no connection to daemon"));
        assertTrue(line.contains("Monero node did not answer over Tor"));
        assertTrue(line.contains("no connection to daemon"));
        assertFalse(line.contains("extra monero-wallet-rpc"));
    }

    @Test
    public void anyOtherCauseIsShownAsItIs() {
        String line = ProcessAsb.xmrInitErrorMessage(new JSONObject().put("error", "wallet file is locked"));
        assertTrue(line.endsWith("wallet file is locked"));
    }

    @Test
    public void anOlderAsbWithoutACauseStillGetsAHelpfulLine() {
        String line = ProcessAsb.xmrInitErrorMessage(new JSONObject());
        assertTrue(line.contains("Check the Monero node in Settings"));
    }

    @Test
    public void theSyncHeartbeatSaysHowLongItHasRunAndThatSlowIsNormal() {
        assertTrue(ProcessAsb.xmrSyncHeartbeatMessage(1).contains("(1 minute so far)"));
        String line = ProcessAsb.xmrSyncHeartbeatMessage(4);
        assertTrue(line.contains("(4 minutes so far)"));
        assertTrue(line.contains("10 minutes or more"));
    }

    @Test
    public void theLiquidityPageSaysTheFirstSyncIsSlowOnlyOnce() {
        assertTrue(ProcessAsb.xmrSyncStatus(0).startsWith("Syncing the ASB's Monero wallet..."));
        String later = ProcessAsb.xmrSyncStatus(3);
        assertTrue(later.contains("3 minutes so far"));
        assertTrue(later.contains("It happens once"));
    }

    @Test
    public void aBitcoinWalletThatCouldNotReachElectrumSaysSo() {
        String line = ProcessAsb.btcInitErrorMessage(new JSONObject().put("error",
                "the Electrum server did not answer over Tor within 180 s; pick another one in Settings"));
        assertTrue(line.startsWith("[ASB] Error initializing Bitcoin wallet: the Electrum server did not answer"));
        assertTrue(ProcessAsb.btcInitErrorMessage(new JSONObject()).contains("Test connection"));
    }

    @Test
    public void theSellerListIsRefreshedOnceAfterTheFirstRegistrationOfARun() {
        boolean[] run = {false};
        assertTrue(ProcessAsb.refreshesSellersAfterRegistering(run));
        assertFalse(ProcessAsb.refreshesSellersAfterRegistering(run));
        assertFalse(ProcessAsb.refreshesSellersAfterRegistering(run));
        assertTrue(ProcessAsb.refreshesSellersAfterRegistering(new boolean[] {false})); // a new ASB run
    }
}
