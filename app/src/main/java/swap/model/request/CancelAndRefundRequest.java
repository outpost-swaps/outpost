package swap.model.request;

import swap.helper.HelperNetwork;
import org.bitcoinj.core.NetworkParameters;
import org.json.JSONObject;

public record CancelAndRefundRequest(String rootBip32Key, String swapId, String electrumUrl, String proxy,
                                     NetworkParameters params) {

    public JSONObject toJson() {
        JSONObject resumeRequestJson = new JSONObject();
        resumeRequestJson.put("rootBip32Key", this.rootBip32Key());
        resumeRequestJson.put("swapId", this.swapId());
        resumeRequestJson.put("electrumUrl", this.electrumUrl());
        resumeRequestJson.put("proxy", this.proxy());
        resumeRequestJson.put("testnet", HelperNetwork.isTestnet(this.params()));
        return resumeRequestJson;
    }
}
