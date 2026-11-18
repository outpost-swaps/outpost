package swap.gui.controller;

/**
 * Works out which paragraphs of the swap log text area a freshly appended
 * message occupies, so each message is colored over exactly its own lines.
 *
 * The log used to color a message with a running per-message counter, but a
 * message containing newlines (for example a multi-line error from the Rust
 * layer) adds several paragraphs, so every later color landed on the wrong
 * line (2026-11-07: the success message's green ended up on the "Still syncing
 * XMR wallet" line).
 */
final class LogParagraphs {

    private LogParagraphs() {
    }

    /**
     * @param paragraphsBefore the text area's paragraph count before appending
     * @param areaWasEmpty     whether the text area had no text before appending
     *                         (the message then goes into the existing empty
     *                         paragraph instead of starting a new one)
     * @param message          the full text that will be appended (without the
     *                         separating newline)
     * @return {first, last}: the inclusive paragraph indexes the message occupies
     */
    static int[] appendedRange(int paragraphsBefore, boolean areaWasEmpty, String message) {
        int first = areaWasEmpty ? 0 : paragraphsBefore;
        int newlines = 0;
        for (int i = 0; i < message.length(); i++) {
            if (message.charAt(i) == '\n') {
                newlines++;
            }
        }
        return new int[]{first, first + newlines};
    }
}
