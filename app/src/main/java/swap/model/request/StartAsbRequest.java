package swap.model.request;

import org.bitcoinj.core.Coin;
import org.bitcoinj.core.NetworkParameters;
import org.json.JSONArray;
import swap.helper.HelperProperties;
import swap.lib.AppAsb;
import swap.model.Multiaddr;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;

public record StartAsbRequest(
        String electrumUrl,
        List<String> electrumPoolUrls,
        String proxyAddress,
        int proxyPort,
        String xmrRpcEndpoint,
        String xmrDaemonUrl,
        List<Multiaddr> libp2pRendezvousAddresses,
        boolean resumeOnly,
        float minQuantity,
        float maxQuantity,
        float fee,
        Coin manualPrice,
        NetworkParameters params,
        String onionAddress) {

    public HashMap<String, List<String>> toConfigToml() {
        String network = HelperProperties.network.toLowerCase();
        JSONArray rendezvousArray = new JSONArray();
        for (Multiaddr libP2pRendezvousAddress : libp2pRendezvousAddresses()) {
            rendezvousArray.put(libP2pRendezvousAddress.toString());
        }
        String dir;
        if (System.getProperty("os.name").toLowerCase().contains("windows")) {
            dir = "dir = \"" + AppAsb.getAsbRootDir().getAbsolutePath() + "\\" + network + "\"";
            dir = dir.replace("\\", "/");
        } else {
            dir = "dir = \"" + AppAsb.getAsbRootDir().getAbsolutePath() + "/" + network + "\"";
        }
        String websocketUrl = "wss://ws.featherwallet.org/ws";
        if(proxyPort() > 0) {
            websocketUrl = "ws://7e6egbawekbkxzkv4244pqeqgoo4axko2imgjbedwnn6s5yb6b7oliqd.onion/ws";
        }

        HashMap<String, List<String>> tomlProperties = new HashMap<>();
        tomlProperties.put("data", List.of(
                dir
        ));
        tomlProperties.put("network", List.of(
                "rendezvous_point = " + rendezvousArray,
                "listen = [\"/ip4/127.0.0.1/tcp/9939\"]",
                "external_addresses = [\"/onion3/" + onionAddress() + ":9939\"]"
        ));
        ArrayList<String> bitcoinProperties = new ArrayList<>(List.of(
                "electrum_rpc_url = \"" + electrumUrl() + "\"",
                "target_block = 1",
                "network = \"" + (network.equals("testnet") ? "Testnet" : "Mainnet") + "\""
        ));
        if (electrumPoolUrls() != null && !electrumPoolUrls().isEmpty()) {
            StringBuilder urls = new StringBuilder();
            for (String url : electrumPoolUrls()) {
                if (urls.length() > 0) {
                    urls.append(", ");
                }
                urls.append('"').append(url).append('"');
            }
            bitcoinProperties.add(1, "electrum_rpc_urls = [" + urls + "]");
        }
        tomlProperties.put("bitcoin", bitcoinProperties);
        tomlProperties.put("monero", List.of(
                "wallet_rpc_url = \"" + xmrRpcEndpoint() + "\"",
                "daemon_url = \"" + xmrDaemonUrl() + "\"",
                "network = \"" + (network.equals("testnet") ? "Stagenet" : "Mainnet") + "\""
        ));
        tomlProperties.put("tor", List.of(
                "control_port = 9051",
                "socks5_port = " + proxyPort()
        ));
        ArrayList<String> makerProperties = new ArrayList<>(List.of(
                "min_buy_btc = " + Coin.parseCoin(minQuantity() + "").toPlainString(),
                "max_buy_btc = " + Coin.parseCoin(maxQuantity() + "").toPlainString(),
                "ask_spread = " + (fee() / 100f),
                "price_ticker_ws_url = \"" + websocketUrl + "\""
        ));
        if (manualPrice() != null) {
            makerProperties.add("manual_price = " + manualPrice().toPlainString());
        }
        tomlProperties.put("maker", makerProperties);

        return tomlProperties;
    }
}
