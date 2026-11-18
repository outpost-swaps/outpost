package swap.gui.controller;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;

import org.junit.Test;

public class LogParagraphsTest {

    @Test
    public void firstMessageGoesIntoTheEmptyParagraph() {
        assertArrayEquals(new int[]{0, 0}, LogParagraphs.appendedRange(1, true, "first"));
    }

    @Test
    public void laterSingleLineMessageStartsANewParagraph() {
        assertArrayEquals(new int[]{5, 5}, LogParagraphs.appendedRange(5, false, "next"));
    }

    @Test
    public void multiLineMessageSpansItsOwnParagraphs() {
        // The swap's Electrum error: header line plus an indented cause line.
        String message = "Redeem not complete yet (Made one or multiple attempts, all errored:\n"
                + "    - Resource temporarily unavailable (os error 35)\n"
                + "); Bitcoin is still locked.";
        assertArrayEquals(new int[]{7, 9}, LogParagraphs.appendedRange(7, false, message));
    }

    // Regression for swap bf5f78fa (2026-11-07): after a multi-line message the
    // old per-message counter drifted, so the SUCCESS color of the last line was
    // applied two paragraphs too early and the last line stayed blue.
    @Test
    public void colorsStayOnTheirOwnLinesAfterAMultiLineMessage() {
        String[] messages = {"start", "error line one\nerror line two\nerror line three", "Still syncing", "Successfully transferred Monero"};
        int paragraphs = 1; // an empty text area holds one empty paragraph
        boolean empty = true;
        int[][] ranges = new int[messages.length][];
        for (int i = 0; i < messages.length; i++) {
            ranges[i] = LogParagraphs.appendedRange(paragraphs, empty, messages[i]);
            assertEquals("the appended text must start where its range says", empty ? 0 : paragraphs, ranges[i][0]);
            paragraphs = ranges[i][1] + 1; // what the text area would now report
            empty = false;
        }
        assertArrayEquals(new int[]{0, 0}, ranges[0]);
        assertArrayEquals(new int[]{1, 3}, ranges[1]);
        assertArrayEquals(new int[]{4, 4}, ranges[2]);
        // The old counter would have colored paragraph 3 here; the line is paragraph 5.
        assertArrayEquals(new int[]{5, 5}, ranges[3]);
    }
}
