package swap.helper;

import java.util.Collection;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Only one client swap may run at a time: every swap spends from the same deposit wallet, so
 * two concurrent swaps could select the same UTXO for their locks. The UI already hides the
 * start button while a swap runs; this makes the rule hold for every caller (first review #11).
 */
public final class HelperSwapGuard {

    private HelperSwapGuard() {}

    public static void requireNoRunningSwap(Collection<AtomicBoolean> runningFlags) {
        for (AtomicBoolean running : runningFlags) {
            if (running.get()) {
                throw new IllegalStateException("A swap is already running; wait for it to finish or stop it first");
            }
        }
    }
}
