package swap.gui.controller.pages;

/** User-visible step details for terminal swap states. */
public final class SwapStepLabels {

    /** Step 4 while the XMR lock is final and the seller has not yet claimed the BTC (§455). */
    public static final String WAITING_FOR_SELLER = "waiting for seller";

    private SwapStepLabels() {
    }

    /**
     * Success-screen vocabulary, identical for live and resumed swaps.
     * WORKLOG §455: "BTC redeemed" reads "claimed" (the seller claimed the BTC; was blank, §264); §262: XMR redeemed is "swept".
     */
    public static String[] completion() {
        return new String[]{"sent", "confirmed", "confirmed", "claimed", "swept"};
    }

    /**
     * What a finished row says when nothing was written for it. A live swap fills a row's text while
     * that step is current; a resumed swap jumps past the early steps and left them blank (seen again
     * 2026-11-14, after the second restart). Same words as {@link #completion()}.
     */
    public static String completedDetail(int stepNumber) {
        String[] words = completion();
        return stepNumber >= 1 && stepNumber <= words.length ? words[stepNumber - 1] : "";
    }
}
