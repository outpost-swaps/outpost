package swap.listener;

import swap.model.AsbBtcBalanceData;
import swap.model.AsbInitData;
import swap.model.AsbXmrBalanceData;
import swap.model.SwapError;

// Called by the native library
public interface AsbListener {
    void onAsbInitialized(AsbInitData data);

    void onAsbXmrBalanceData(AsbXmrBalanceData data);

    void onAsbBtcBalanceData(String accountName, AsbBtcBalanceData data);

    void onAsbError(SwapError swapError);
}
