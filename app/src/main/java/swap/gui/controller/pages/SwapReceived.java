package swap.gui.controller.pages;

import java.math.BigDecimal;

/**
 * The Swap Success amount line (matrix N-15): the exact XMR the sweep paid and its network fee,
 * as reported by the engine. Nothing here estimates; without a report the screen shows no number.
 */
public final class SwapReceived {

    private static final int PICONERO_DECIMALS = 12;

    private SwapReceived() {
    }

    /** Piconero as XMR with every significant digit and no trailing zeros, e.g. "0.12517824". */
    public static String xmr(long piconero) {
        if (piconero < 0) throw new IllegalArgumentException("negative amount: " + piconero);
        if (piconero == 0) return "0";
        return new BigDecimal(piconero).movePointLeft(PICONERO_DECIMALS).stripTrailingZeros().toPlainString();
    }

    /** The big line. */
    public static String amountLine(long receivedPiconero) {
        return xmr(receivedPiconero) + " XMR";
    }

    /** The small line under it. */
    public static String caption(long feePiconero) {
        return "Network fee: " + xmr(feePiconero) + " XMR";
    }

    /** The log line written when the swap completes. */
    public static String logLine(long receivedPiconero, long feePiconero) {
        return "Received " + amountLine(receivedPiconero) + " (network fee " + xmr(feePiconero) + " XMR).";
    }
}
