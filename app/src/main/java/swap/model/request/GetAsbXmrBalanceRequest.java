package swap.model.request;

import swap.helper.HelperNetwork;
import org.bitcoinj.core.NetworkParameters;
import org.json.JSONObject;

public record GetAsbXmrBalanceRequest(
        String xmrRpcEndpoint,
        NetworkParameters params) {

    public JSONObject toJson() {
        JSONObject startAsbRequestJson = new JSONObject();
        startAsbRequestJson.put("xmrRpcEndpoint", this.xmrRpcEndpoint());
        startAsbRequestJson.put("testnet", HelperNetwork.isTestnet(this.params()));
        return startAsbRequestJson;
    }
}
