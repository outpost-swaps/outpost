package swap.model.request;

import swap.helper.HelperNetwork;
import org.bitcoinj.core.NetworkParameters;
import org.json.JSONObject;

import javax.annotation.Nullable;
import java.util.List;

public record ResumeRequest(String seedBase64, String swapId, String electrumUrl, String proxy,
                            @Nullable String xmrRpcEndpoint,
                            NetworkParameters params, int proxyPort, long swapsAccount,
                            int swapsReceiveIndex, int swapsChangeIndex,
                            @Nullable String dojoUrl, @Nullable String dojoApiKey, @Nullable String xmrDaemonUrl,
                            List<String> electrumPoolUrls,
                            List<String> xmrWitnessUrls, boolean xmrWitnessRequired, int xmrWitnessCount) {
    /** Extra Electrum endpoints the wallet may fail over to; never null. */
    public ResumeRequest {
        electrumPoolUrls = electrumPoolUrls == null ? List.of() : List.copyOf(electrumPoolUrls);
        xmrWitnessUrls = xmrWitnessUrls == null ? List.of() : List.copyOf(xmrWitnessUrls);
    }

    /** Candidates given without a count (older callers): used as given, no pre-flight. */
    public ResumeRequest(String seedBase64,
            String swapId,
            String electrumUrl,
            String proxy,
            @Nullable String xmrRpcEndpoint,
            NetworkParameters params,
            int proxyPort,
            long swapsAccount,
            int swapsReceiveIndex,
            int swapsChangeIndex,
            @Nullable String dojoUrl,
            @Nullable String dojoApiKey,
            @Nullable String xmrDaemonUrl,
            List<String> electrumPoolUrls,
            List<String> xmrWitnessUrls,
            boolean xmrWitnessRequired) {
        this(seedBase64, swapId, electrumUrl, proxy, xmrRpcEndpoint, params, proxyPort, swapsAccount, swapsReceiveIndex, swapsChangeIndex, dojoUrl, dojoApiKey, xmrDaemonUrl, electrumPoolUrls, xmrWitnessUrls, xmrWitnessRequired, 0);
    }

    /** Without witnesses (the older shape): none configured and none required. */
    public ResumeRequest(String seedBase64, String swapId, String electrumUrl, String proxy,
                         @Nullable String xmrRpcEndpoint,
                         NetworkParameters params, int proxyPort, long swapsAccount,
                         int swapsReceiveIndex, int swapsChangeIndex,
                         @Nullable String dojoUrl, @Nullable String dojoApiKey, @Nullable String xmrDaemonUrl,
                         List<String> electrumPoolUrls) {
        this(seedBase64, swapId, electrumUrl, proxy, xmrRpcEndpoint, params, proxyPort, swapsAccount,
                swapsReceiveIndex, swapsChangeIndex, dojoUrl, dojoApiKey, xmrDaemonUrl, electrumPoolUrls,
                List.of(), false);
    }

    public ResumeRequest(String seedBase64, String swapId, String electrumUrl, String proxy,
                         @Nullable String xmrRpcEndpoint,
                         NetworkParameters params, int proxyPort, long swapsAccount,
                         int swapsReceiveIndex, int swapsChangeIndex,
                         @Nullable String dojoUrl, @Nullable String dojoApiKey, @Nullable String xmrDaemonUrl) {
        this(seedBase64, swapId, electrumUrl, proxy, xmrRpcEndpoint, params, proxyPort, swapsAccount,
                swapsReceiveIndex, swapsChangeIndex, dojoUrl, dojoApiKey, xmrDaemonUrl, List.of());
    }

    public ResumeRequest(String seedBase64, String swapId, String electrumUrl, String proxy,
                         NetworkParameters params, int proxyPort, long swapsAccount,
                         int swapsReceiveIndex, int swapsChangeIndex,
                         @Nullable String dojoUrl, @Nullable String dojoApiKey) {
        this(seedBase64, swapId, electrumUrl, proxy, null, params, proxyPort, swapsAccount, swapsReceiveIndex, swapsChangeIndex, dojoUrl, dojoApiKey, null);
    }

    public JSONObject toJson() {
        JSONObject resumeRequestJson = new JSONObject();
        resumeRequestJson.put("seedBase64", this.seedBase64());
        resumeRequestJson.put("swapId", this.swapId());
        resumeRequestJson.put("electrumUrl", this.electrumUrl());
        resumeRequestJson.put("proxy", this.proxy());
        resumeRequestJson.put("xmrRpcEndpoint", this.xmrRpcEndpoint());
        resumeRequestJson.put("testnet", HelperNetwork.isTestnet(this.params()));
        resumeRequestJson.put("proxyPort", this.proxyPort());
        resumeRequestJson.put("swapsAccount", this.swapsAccount());
        resumeRequestJson.put("swapsReceiveIndex", this.swapsReceiveIndex());
        resumeRequestJson.put("swapsChangeIndex", this.swapsChangeIndex());
        resumeRequestJson.put("dojoUrl", this.dojoUrl());
        resumeRequestJson.put("dojoApiKey", this.dojoApiKey());
        resumeRequestJson.put("xmrDaemonUrl", this.xmrDaemonUrl());
        resumeRequestJson.put("electrumPoolUrls", new org.json.JSONArray(this.electrumPoolUrls()));
        resumeRequestJson.put("xmrWitnessUrls", new org.json.JSONArray(this.xmrWitnessUrls()));
        resumeRequestJson.put("xmrWitnessRequired", this.xmrWitnessRequired());
        resumeRequestJson.put("xmrWitnessCount", this.xmrWitnessCount());
        return resumeRequestJson;
    }
}
