package swap.gui.controller.pages;

/** When the swap panel's "Disconnect" button must warn first (audit M-5). */
public final class DisconnectPolicy {
    private DisconnectPolicy() {}

    /**
     * Always while a swap runs, unless it is known to be before the Bitcoin lock (still waiting
     * for, or just received, the deposit). From "negotiated" on the lock may be published at any
     * moment; an unknown step warns too.
     */
    public static boolean needsWarning(boolean swapRunning, int step, String detail) {
        if (!swapRunning) return false;
        boolean beforeLock = step == 1
                && ("waiting for deposit".equals(detail) || "deposit received".equals(detail));
        return !beforeLock;
    }
}
