package swap.process;

/**
 * What the Liquidity page's "In progress" list says about a swap, in plain words from the seller's
 * side. The engine's own state names ("xmr lock transfer proof sent") stay in the logs.
 */
public final class AsbStepLabels {
    public static final String ACCEPTED = "Setup";
    public static final String BTC_SEEN = "BTC lock seen";
    public static final String BTC_NOT_SEEN = "BTC lock not seen";
    public static final String BTC_CONFIRMED = "BTC locked";
    public static final String BTC_NOT_CONFIRMED = "BTC lock not confirmed";
    public static final String XMR_LOCKING = "XMR lock sent";
    public static final String XMR_LOCKED = "XMR locked";
    public static final String XMR_WAIT_BUYER = "XMR confirming";
    public static final String XMR_CONFIRMING = "XMR confirming";
    public static final String XMR_RETRYING = "XMR lock retrying";
    public static final String XMR_FOUND = "XMR lock reconciled";
    public static final String RESERVED = "Reserved";
    public static final String SIGNED = "Buyer signed";
    public static final String CLAIMING = "Redeeming BTC";
    public static final String CLAIM_FAILED = "Redeem failed";
    public static final String CLAIMED = "Redeemed";
    public static final String CANCELLING = "Cancelling";
    public static final String PUNISHING = "Punishing";

    private AsbStepLabels() {}

    /** The label for an engine state name; unknown names pass through unchanged. */
    public static String forState(String state) {
        if (state == null || state.isBlank()) {
            return "Running";
        }
        return switch (state) {
            case "started" -> ACCEPTED;
            case "bitcoin lock transaction in mempool" -> BTC_SEEN;
            case "btc is locked" -> BTC_CONFIRMED;
            case "xmr lock transaction sent" -> XMR_LOCKING;
            case "xmr is locked" -> XMR_LOCKED;
            case "xmr lock transfer proof sent" -> XMR_WAIT_BUYER;
            case "encrypted signature is learned" -> SIGNED;
            case "bitcoin redeem transaction published" -> CLAIMING;
            case "btc is redeemed" -> CLAIMED;
            case "btc is cancelled" -> "Cancelled";
            case "btc is refunded" -> "Refunded";
            case "xmr is refunded" -> "XMR refunded";
            case "btc is early refunded" -> "Early refunded";
            case "safely aborted" -> "Aborted";
            default -> state;
        };
    }
}
