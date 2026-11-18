package swap.helper;

import java.math.BigDecimal;
import java.math.RoundingMode;
import java.util.List;
import java.util.Locale;
import swap.model.Seller;

/**
 * The two-way amount preview on the Swaps page, free of any window so it can be tested.
 * It is only a guide: the real amount is set after the deposit, from what arrives.
 * The Bitcoin lock fee and the Monero sweep fee are flat assumptions, small next to the swap.
 */
public final class HelperSwapEstimate {
    /** About 200 vB at roughly 5 sat/vB, in BTC. */
    static final double ASSUMED_LOCK_FEE_BTC = 0.00001;
    /** A typical Monero sweep fee, in XMR. */
    static final double ASSUMED_SWEEP_FEE_XMR = 0.00005;

    public static final String NO_SELLER = "pick a seller";
    public static final String BELOW_MIN = "below min";
    public static final String ABOVE_MAX = "above max";
    public static final String TOO_SMALL = "too small";

    private HelperSwapEstimate() {}

    /** Both sides of the estimate; the side the user did not type is filled when it can be. */
    public record Conversion(Double btc, Double xmr, String note) {
        public boolean ok() {
            return note.isEmpty();
        }
    }

    /** BTC the user pays -> XMR they should receive. */
    public static Conversion fromBtc(String amountText, Double minBtc, Double maxBtc, Double priceBtcPerXmr) {
        Double btc = parse(amountText);
        if (btc == null) {
            return new Conversion(null, null, "");
        }
        String note = rangeNote(btc, minBtc, maxBtc);
        if (!note.isEmpty()) {
            return new Conversion(btc, null, note);
        }
        if (priceBtcPerXmr == null || priceBtcPerXmr <= 0) {
            return new Conversion(btc, null, NO_SELLER);
        }
        double xmr = (btc - ASSUMED_LOCK_FEE_BTC) / priceBtcPerXmr - ASSUMED_SWEEP_FEE_XMR;
        if (xmr <= 0) {
            return new Conversion(btc, null, TOO_SMALL);
        }
        return new Conversion(btc, xmr, "");
    }

    /** XMR the user wants -> BTC they would need to send. */
    public static Conversion fromXmr(String amountText, Double minBtc, Double maxBtc, Double priceBtcPerXmr) {
        Double xmr = parse(amountText);
        if (xmr == null) {
            return new Conversion(null, null, "");
        }
        if (priceBtcPerXmr == null || priceBtcPerXmr <= 0) {
            return new Conversion(null, xmr, NO_SELLER);
        }
        double btc = (xmr + ASSUMED_SWEEP_FEE_XMR) * priceBtcPerXmr + ASSUMED_LOCK_FEE_BTC;
        String note = rangeNote(btc, minBtc, maxBtc);
        if (!note.isEmpty()) {
            return new Conversion(btc, xmr, note);
        }
        return new Conversion(btc, xmr, "");
    }

    /**
     * The seller the estimate prices against: the highlighted table row, else the listed seller whose
     * address is in the seller field. A seller-list refresh clears the table selection while the field
     * keeps the address, and the estimate used to stop (2026-11-14: "Pick a seller first" with a seller set).
     */
    public static Seller sellerFor(Seller selected, String peerText, List<Seller> sellers) {
        if (selected != null) {
            return selected;
        }
        if (peerText == null || peerText.isBlank() || sellers == null) {
            return null;
        }
        String peer = peerText.trim();
        synchronized (sellers) {
            for (Seller seller : sellers) {
                if (peer.equals(seller.multiaddr())) {
                    return seller;
                }
            }
        }
        return null;
    }

    /** Eight decimals, trailing zeros trimmed, for the Bitcoin field. */
    public static String formatBtc(Double btc) {
        if (btc == null || btc <= 0 || Double.isInfinite(btc)) {
            return "";
        }
        return BigDecimal.valueOf(btc).setScale(8, RoundingMode.HALF_UP).stripTrailingZeros().toPlainString();
    }

    /** Six decimals for the Monero field. */
    public static String formatXmr(Double xmr) {
        if (xmr == null || xmr <= 0 || Double.isInfinite(xmr)) {
            return "";
        }
        return String.format(Locale.ROOT, "%.6f", xmr);
    }

    private static String rangeNote(double btc, Double minBtc, Double maxBtc) {
        if (minBtc != null && btc < minBtc) {
            return BELOW_MIN;
        }
        if (maxBtc != null && btc > maxBtc) {
            return ABOVE_MAX;
        }
        return "";
    }

    /** A positive amount, or null for empty or unreadable text. */
    static Double parse(String text) {
        if (text == null || text.isBlank()) {
            return null;
        }
        try {
            double value = Double.parseDouble(text.trim().replace(',', '.'));
            return value > 0 && !Double.isInfinite(value) ? value : null;
        } catch (NumberFormatException e) {
            return null;
        }
    }
}
