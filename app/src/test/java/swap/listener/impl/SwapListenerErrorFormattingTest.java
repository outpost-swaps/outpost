package swap.listener.impl;

import org.junit.Test;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;

public class SwapListenerErrorFormattingTest {

    private static final String NATIVE_ERROR =
            "Failed to redeem XMR: wallet RPC error\n"
                    + "Caused by: error sending request for url (http://127.0.0.1:51460/json_rpc)\n"
                    + "Caused by: connection refused\n"
                    + "Stack backtrace:\n"
                    + "   0: std::backtrace_rs::backtrace::libunwind::trace\n"
                    + "   1: anyhow::error::<impl core::convert::From<E> for anyhow::Error>::from";

    @Test
    public void logKeepsActualErrorButDropsCauseAndStack() {
        String log = SwapListenerImpl.logError(NATIVE_ERROR);
        assertEquals("Failed to redeem XMR: wallet RPC error", log);
        assertFalse(log.contains("Caused by"));
        assertFalse(log.contains("Stack backtrace"));
        assertFalse(log.contains("\n"));
        System.out.println("LOG: " + log);
    }

    @Test
    public void uiKeepsShortActualError() {
        String ui = SwapListenerImpl.userFacingError(NATIVE_ERROR);
        assertEquals("Failed to redeem XMR: wallet RPC error", ui);
        System.out.println("UI: " + ui);
    }

    @Test
    public void uiErrorStaysShortWithoutEllipsis() {
        String longError = "Failed to redeem XMR: " + "x".repeat(220);
        String ui = SwapListenerImpl.userFacingError(longError);
        String log = SwapListenerImpl.logError(longError);
        assertEquals("Unexpected swap error.", ui);
        assertFalse(ui.contains("..."));
        assertEquals(longError, log);
        assertFalse(log.contains("..."));
        System.out.println("LONG_UI: " + ui);
        System.out.println("LONG_LOG: " + log);
    }

    @Test
    public void sellerSetupFailuresSayNothingWasLockedAndSuggestAnotherSeller() {
        String refused = SwapListenerImpl.userFacingError(
                "Protocol-5 setup failed: Swap rejected: seller's offer refused: the seller offers 26504552744 "
                        + "piconero for 39500 sat, but its quote implies 52311506412");
        assertEquals("Seller's offer didn't match its quote. Nothing locked. Try another seller.", refused);

        String problem = SwapListenerImpl.userFacingError(
                "Protocol-5 setup failed: Something went wrong during the swap setup protocol: "
                        + "Seller encountered a problem, please try again later.");
        assertEquals("Seller had a problem on its side. Nothing locked. Try another seller.", problem);

        assertEquals("Amount is outside this seller's limits. Try another seller.",
                SwapListenerImpl.userFacingError(
                        "Seller refused to buy 0.0001 BTC because the minimum configured buy limit is 0.0005 BTC"));
        assertEquals("This seller has too little Monero. Try another seller.",
                SwapListenerImpl.userFacingError(
                        "Seller's XMR balance is currently too low to fulfill the swap request to buy 0.5 BTC, please try again later"));
        assertEquals("This seller isn't accepting swaps. Try another seller.",
                SwapListenerImpl.userFacingError(
                        "Seller currently does not accept incoming swap requests, please try again later"));
    }

    @Test
    public void everySellerMessageFitsTheStatusBox() {
        for (String text : new String[] {
                "Seller's offer didn't match its quote. Nothing locked. Try another seller.",
                "Seller had a problem on its side. Nothing locked. Try another seller.",
                "Amount is outside this seller's limits. Try another seller."}) {
            assertEquals(true, text.length() <= 80);
        }
    }
}
