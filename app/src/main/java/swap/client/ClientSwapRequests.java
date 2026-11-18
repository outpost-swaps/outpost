package swap.client;

import org.json.JSONObject;
import swap.model.request.SwapRequest;

import javax.annotation.Nullable;

/** Builds the JSON handed to the native buy entry point. */
public final class ClientSwapRequests {

    private ClientSwapRequests() {
    }

    /** The buy request, with the seller fee rate the user accepted for this swap when there is one (S-01). */
    public static JSONObject buy(SwapRequest request, @Nullable Long acceptedSellerFeeRate) {
        JSONObject json = request.toJson();
        if (acceptedSellerFeeRate != null) json.put("acceptedSellerFeeRateSatKvb", acceptedSellerFeeRate.longValue());
        return json;
    }
}
