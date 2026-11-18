package swap.model.response;

import org.json.JSONArray;
import org.json.JSONObject;
import swap.model.SwapData;

import java.util.ArrayList;
import java.util.List;

public class GetHistoryResponse {
    private final List<SwapData> swaps;

    public GetHistoryResponse(List<SwapData> sellers) {
        this.swaps = sellers;
    }

    public static GetHistoryResponse empty() {
        return new GetHistoryResponse(new ArrayList<>());
    }

    public static GetHistoryResponse fromJson(String json) {
        ArrayList<SwapData> swaps = new ArrayList<>();
        JSONObject responseJson = new JSONObject(json);
        JSONArray swapsArray = responseJson.getJSONArray("swaps");
        for (int i = 0; i < swapsArray.length(); i++) {
            JSONObject swapJson = swapsArray.getJSONObject(i);
            String swapId = swapJson.getString("swapId");
            String status = swapJson.getString("status");
            String date = swapJson.optString("date", "");
            swaps.add(new SwapData(swapId, status, date));
        }
        return new GetHistoryResponse(swaps);
    }

    public List<SwapData> getSwaps() {
        return swaps;
    }
}
