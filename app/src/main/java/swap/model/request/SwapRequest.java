package swap.model.request;

import swap.helper.HelperNetwork;
import org.bitcoinj.core.NetworkParameters;
import org.json.JSONObject;

import javax.annotation.Nullable;
import java.util.List;

public record SwapRequest(String uuid, String seedBase64, String xmrReceiveAddress, String electrumUrl,
                          String libp2pPeerAddress,
                          String proxy, @Nullable String xmrRpcEndpoint, NetworkParameters params, int proxyPort,
                          String refundAddress,
                          long swapsAccount, int swapsReceiveIndex, int swapsChangeIndex,
                          @Nullable String dojoUrl, @Nullable String dojoApiKey, @Nullable String xmrDaemonUrl,
                          List<String> electrumPoolUrls,
                          List<String> xmrWitnessUrls, boolean xmrWitnessRequired, int xmrWitnessCount) {
    /** Extra Electrum endpoints the wallet may fail over to; never null. */
    public SwapRequest {
        electrumPoolUrls = electrumPoolUrls == null ? List.of() : List.copyOf(electrumPoolUrls);
        xmrWitnessUrls = xmrWitnessUrls == null ? List.of() : List.copyOf(xmrWitnessUrls);
    }

    /** Candidates given without a count (older callers): used as given, no pre-flight. */
    public SwapRequest(String uuid,
            String seedBase64,
            String xmrReceiveAddress,
            String electrumUrl,
            String libp2pPeerAddress,
            String proxy,
            @Nullable String xmrRpcEndpoint,
            NetworkParameters params,
            int proxyPort,
            String refundAddress,
            long swapsAccount,
            int swapsReceiveIndex,
            int swapsChangeIndex,
            @Nullable String dojoUrl,
            @Nullable String dojoApiKey,
            @Nullable String xmrDaemonUrl,
            List<String> electrumPoolUrls,
            List<String> xmrWitnessUrls,
            boolean xmrWitnessRequired) {
        this(uuid, seedBase64, xmrReceiveAddress, electrumUrl, libp2pPeerAddress, proxy, xmrRpcEndpoint, params, proxyPort, refundAddress, swapsAccount, swapsReceiveIndex, swapsChangeIndex, dojoUrl, dojoApiKey, xmrDaemonUrl, electrumPoolUrls, xmrWitnessUrls, xmrWitnessRequired, 0);
    }

    /** Without witnesses (the older shape): none configured and none required. */
    public SwapRequest(String uuid, String seedBase64, String xmrReceiveAddress, String electrumUrl,
                       String libp2pPeerAddress,
                       String proxy, @Nullable String xmrRpcEndpoint, NetworkParameters params, int proxyPort,
                       String refundAddress,
                       long swapsAccount, int swapsReceiveIndex, int swapsChangeIndex,
                       @Nullable String dojoUrl, @Nullable String dojoApiKey, @Nullable String xmrDaemonUrl,
                       List<String> electrumPoolUrls) {
        this(uuid, seedBase64, xmrReceiveAddress, electrumUrl, libp2pPeerAddress, proxy, xmrRpcEndpoint, params,
                proxyPort, refundAddress, swapsAccount, swapsReceiveIndex, swapsChangeIndex, dojoUrl, dojoApiKey,
                xmrDaemonUrl, electrumPoolUrls, List.of(), false);
    }

    public SwapRequest(String uuid, String seedBase64, String xmrReceiveAddress, String electrumUrl,
                       String libp2pPeerAddress,
                       String proxy, @Nullable String xmrRpcEndpoint, NetworkParameters params, int proxyPort,
                       String refundAddress,
                       long swapsAccount, int swapsReceiveIndex, int swapsChangeIndex,
                       @Nullable String dojoUrl, @Nullable String dojoApiKey, @Nullable String xmrDaemonUrl) {
        this(uuid, seedBase64, xmrReceiveAddress, electrumUrl, libp2pPeerAddress, proxy, xmrRpcEndpoint, params,
                proxyPort, refundAddress, swapsAccount, swapsReceiveIndex, swapsChangeIndex, dojoUrl, dojoApiKey,
                xmrDaemonUrl, List.of());
    }

    public SwapRequest(String uuid, String seedBase64, String xmrReceiveAddress, String electrumUrl, String libp2pPeerAddress,
                       String proxy, NetworkParameters params, int proxyPort,
                       String refundAddress, long swapsAccount, int swapsReceiveIndex, int swapsChangeIndex,
                       @Nullable String dojoUrl, @Nullable String dojoApiKey) {
        this(uuid, seedBase64, xmrReceiveAddress, electrumUrl, libp2pPeerAddress, proxy, null, params, proxyPort, refundAddress, swapsAccount, swapsReceiveIndex, swapsChangeIndex, dojoUrl, dojoApiKey, null);
    }

    public JSONObject toJson() {
        JSONObject swapRequestJson = new JSONObject();
        swapRequestJson.put("uuid", uuid());
        swapRequestJson.put("seedBase64", this.seedBase64());
        swapRequestJson.put("xmrReceiveAddress", this.xmrReceiveAddress());
        swapRequestJson.put("electrumUrl", this.electrumUrl());
        swapRequestJson.put("proxy", this.proxy());
        swapRequestJson.put("libp2pPeerAddress", this.libp2pPeerAddress());
        swapRequestJson.put("xmrRpcEndpoint", this.xmrRpcEndpoint());
        swapRequestJson.put("testnet", HelperNetwork.isTestnet(this.params()));
        swapRequestJson.put("proxyPort", this.proxyPort());
        swapRequestJson.put("refundAddress", this.refundAddress());
        swapRequestJson.put("swapsAccount", this.swapsAccount());
        swapRequestJson.put("swapsReceiveIndex", this.swapsReceiveIndex());
        swapRequestJson.put("swapsChangeIndex", this.swapsChangeIndex());
        swapRequestJson.put("dojoUrl", this.dojoUrl());
        swapRequestJson.put("dojoApiKey", this.dojoApiKey());
        swapRequestJson.put("xmrDaemonUrl", this.xmrDaemonUrl());
        swapRequestJson.put("electrumPoolUrls", new org.json.JSONArray(this.electrumPoolUrls()));
        swapRequestJson.put("xmrWitnessUrls", new org.json.JSONArray(this.xmrWitnessUrls()));
        swapRequestJson.put("xmrWitnessRequired", this.xmrWitnessRequired());
        swapRequestJson.put("xmrWitnessCount", this.xmrWitnessCount());
        return swapRequestJson;
    }
}
