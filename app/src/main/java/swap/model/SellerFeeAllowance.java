package swap.model;

import java.util.Locale;

/**
 * "Accept and retry" for a seller whose swap fee rate is above our limit (matrix S-01, WORKLOG
 * §458). The buyer refuses a seller asking more than 3x our own fee estimate (the fee-griefing
 * defense kept in §258); this lets the user accept that one seller's rate for ONE new swap.
 * Nothing is saved: the allowance is taken by the next swap to that seller and is then gone.
 */
public final class SellerFeeAllowance {

    /** The largest presigned v5 transaction (cancel), as in docs/FEES.md. */
    static final long MAX_TEMPLATE_VBYTES = 181;

    private static String allowedPeer;
    private static long allowedRateSatPerKvb;
    private static long swapAmountSats;

    private SellerFeeAllowance() {
    }

    /**
     * Remember the rate the seller asked. The engine allows it plus a 10% margin
     * (`seller_fee::accepted_limit`) and reserves the lock fee it forces.
     */
    public static synchronized void allowOnce(String peerAddress, long sellerRateSatPerKvb) {
        allowedPeer = peerAddress;
        allowedRateSatPerKvb = sellerRateSatPerKvb;
    }

    /** The accepted (asked) rate for a new swap to this seller, or {@code null}; either way it is used up. */
    public static synchronized Long take(String peerAddress) {
        Long rate = peerAddress != null && peerAddress.equals(allowedPeer) ? allowedRateSatPerKvb : null;
        allowedPeer = null;
        allowedRateSatPerKvb = 0;
        return rate;
    }

    /** The swap amount in sats once known, so the dialog can show the cost as a share of it. */
    public static synchronized void noteSwapAmount(long sats) {
        swapAmountSats = sats;
    }

    static synchronized long swapAmount() {
        return swapAmountSats;
    }

    /** Longest refund path: cancel, refund and the two deposit transactions. */
    static final int WORST_PATH_TXS = 4;

    /** The most a failed swap can cost in fees at the seller's rate, all of it on the refund path. */
    public static long worstRefundCost(long sellerRateSatPerKvb) {
        return (long) Math.ceil(sellerRateSatPerKvb * MAX_TEMPLATE_VBYTES / 1000.0) * WORST_PATH_TXS;
    }

    /** "~1,448 sats (0.3% of this swap)", or only the sats when the amount is not known. */
    public static String worstRefundText(long sellerRateSatPerKvb, long swapSats) {
        long cost = worstRefundCost(sellerRateSatPerKvb);
        String sats = String.format(Locale.ROOT, "%,d sats", cost);
        if (swapSats <= 0) {
            return "~" + sats;
        }
        return "~" + sats + String.format(Locale.ROOT, " (%.2f%% of this swap)", cost * 100.0 / swapSats);
    }

    public static String satPerVbyte(long satPerKvb) {
        return String.format(Locale.ROOT, "%.1f", satPerKvb / 1000.0);
    }

    /** Fee rates are estimates and the seller's may move a little: shown as "~6.1 sat/vB". */
    public static String approxRate(long satPerKvb) {
        return "~" + satPerVbyte(satPerKvb) + " sat/vB";
    }

    /** About what the seller's rate adds if the swap fails and is refunded (cancel + refund). */
    public static long extraIfRefunded(long sellerRateSatPerKvb, long ourEstimateSatPerKvb) {
        long perTx = (long) Math.ceil(Math.max(0, sellerRateSatPerKvb - ourEstimateSatPerKvb) * MAX_TEMPLATE_VBYTES / 1000.0);
        return perTx * 2;
    }

    public static String title() {
        return "This seller's fees are higher than the network needs";
    }

    public static String summary(long sellerRate, long ourMax, long ourEstimate) {
        return "The seller wants its swap transactions priced at " + approxRate(sellerRate) + ". "
                + "The network currently needs " + approxRate(ourEstimate) + ", and Outpost accepts up to "
                + approxRate(ourMax) + " to protect you, so the swap was stopped before any Bitcoin was locked.";
    }

    public static String costTitle() {
        return "ONLY A FAILED SWAP COSTS MORE";
    }

    public static String costNote(long sellerRate, long ourEstimate) {
        return "A successful swap costs you nothing extra: the seller pays its own redeem fee. You only pay this rate "
                + "if the swap fails and is refunded: ~" + extraIfRefunded(sellerRate, ourEstimate)
                + " sats more. The worst case, with a deposit, is " + worstRefundText(sellerRate, swapAmount())
                + " in fees. Accepting applies to the next try with this seller only.";
    }

    public static String declineLabel() {
        return "Try another seller";
    }

    public static String acceptLabel(long sellerRate) {
        return "Accept " + approxRate(sellerRate) + " and retry";
    }
}
