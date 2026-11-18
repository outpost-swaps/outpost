package swap.model.request;

import swap.helper.HelperNetwork;
import org.bitcoinj.core.NetworkParameters;
import org.json.JSONObject;

public record GetHistoryRequest(NetworkParameters params) {

    public JSONObject toJson() {
        JSONObject swapRequestJson = new JSONObject();
        swapRequestJson.put("testnet", HelperNetwork.isTestnet(this.params()));
        return swapRequestJson;
    }
}
