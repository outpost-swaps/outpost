package swap.model.request;

import swap.helper.HelperNetwork;
import org.bitcoinj.core.NetworkParameters;
import org.json.JSONObject;

import java.util.List;

public record ListSellersDirectRequest(String seedBase64, List<String> makerAddresses, int proxyPort,
                                       NetworkParameters params) {

    public JSONObject toJson() {
        JSONObject swapRequestJson = new JSONObject();
        swapRequestJson.put("seedBase64", this.seedBase64());
        swapRequestJson.put("proxyPort", this.proxyPort());
        swapRequestJson.put("makerAddresses", this.makerAddresses());
        swapRequestJson.put("testnet", HelperNetwork.isTestnet(this.params()));
        return swapRequestJson;
    }
}
