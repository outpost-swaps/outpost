package swap.helper;

import org.json.JSONArray;
import org.json.JSONObject;
import swap.lib.AppSwap;
import swap.model.ElectrumServer;
import swap.model.XmrNode;

import java.io.File;
import java.io.FileInputStream;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.List;

/**
 * The curated daemon pools (electrum for BTC, monero for XMR), loaded from
 * {@code node-directory.json}.
 *
 * Source precedence: a {@code node-directory.json} in the app data dir
 * (editable without rebuilding, and the future merge point for dynamic
 * directory sources — dojos-testnet.json, mineracks services.json,
 * monero.fail api) falls back to the packaged resource shipped in the jar.
 */
public class NodeDirectory {

    private static final String RESOURCE_PATH = "/node-directory.json";
    private static final String OVERRIDE_FILE = "node-directory.json";

    private static NodeDirectory instance;

    private final JSONObject root;
    private final String source;

    private NodeDirectory(JSONObject root, String source) {
        this.root = root;
        this.source = source;
    }

    public static synchronized NodeDirectory getInstance() {
        if (instance == null) {
            instance = load();
        }
        return instance;
    }

    public String source() {
        return source;
    }

    public static List<ElectrumServer> electrum(ElectrumServer.Network network, boolean onion) {
        return getInstance().electrumList(network, onion);
    }

    public static List<XmrNode> monero(XmrNode.Network network, boolean onion) {
        return getInstance().moneroList(network, onion);
    }

    private List<ElectrumServer> electrumList(ElectrumServer.Network network, boolean onion) {
        List<ElectrumServer> servers = new ArrayList<>();
        JSONArray entries = root
                .optJSONObject("electrum")
                .optJSONObject(network == ElectrumServer.Network.MAINNET ? "mainnet" : "testnet")
                .optJSONArray(onion ? "onion" : "clearnet");
        if (entries == null) return servers;
        for (int i = 0; i < entries.length(); i++) {
            JSONObject entry = entries.optJSONObject(i);
            if (entry == null) continue;
            String url = entry.optString("url", "");
            if (url.isEmpty()) continue;
            String type = entry.optString("type", onion ? "ONION" : "WEB_CORS");
            ElectrumServer.Type serverType;
            try {
                serverType = ElectrumServer.Type.valueOf(type);
            } catch (IllegalArgumentException e) {
                serverType = onion ? ElectrumServer.Type.ONION : ElectrumServer.Type.WEB_CORS;
            }
            servers.add(new ElectrumServer(
                    entry.isNull("name") ? null : entry.optString("name", null),
                    url,
                    serverType,
                    network));
        }
        return servers;
    }

    private List<XmrNode> moneroList(XmrNode.Network network, boolean onion) {
        List<XmrNode> nodes = new ArrayList<>();
        String networkKey = network == XmrNode.Network.MAINNET ? "mainnet" : "stagenet";
        JSONArray entries = root
                .optJSONObject("monero")
                .optJSONObject(networkKey)
                .optJSONArray(onion ? "onion" : "clearnet");
        if (entries == null) return nodes;
        for (int i = 0; i < entries.length(); i++) {
            JSONObject entry = entries.optJSONObject(i);
            if (entry == null) continue;
            String url = entry.optString("url", "");
            if (url.isEmpty()) continue;
            String type = entry.optString("type", onion ? "ONION" : "WEB_CORS");
            XmrNode.Type nodeType;
            try {
                nodeType = XmrNode.Type.valueOf(type);
            } catch (IllegalArgumentException e) {
                nodeType = onion ? XmrNode.Type.ONION : XmrNode.Type.WEB_CORS;
            }
            nodes.add(new XmrNode(
                    entry.isNull("name") ? null : entry.optString("name", null),
                    url,
                    nodeType,
                    network));
        }
        return nodes;
    }

    private static NodeDirectory load() {
        // 1. user override in the app data dir
        try {
            File override = new File(AppSwap.getSwapRootDir(), OVERRIDE_FILE);
            if (override.isFile()) {
                JSONObject parsed = new JSONObject(new JSONTokenerReader(new FileInputStream(override)));
                return new NodeDirectory(parsed, override.getAbsolutePath());
            }
        } catch (Exception e) {
            System.out.println("node-directory override unreadable, falling back to packaged: " + e);
        }

        // 2. packaged resource
        try (InputStream in = NodeDirectory.class.getResourceAsStream(RESOURCE_PATH)) {
            if (in != null) {
                return new NodeDirectory(new JSONObject(new JSONTokenerReader(in)), "packaged:" + RESOURCE_PATH);
            }
        } catch (Exception e) {
            System.out.println("Packaged node-directory.json missing/unreadable: " + e);
        }

        return new NodeDirectory(new JSONObject(), "empty");
    }

    /** Thin alias so the two loader paths share one call shape. */
    private static final class JSONTokenerReader extends org.json.JSONTokener {
        JSONTokenerReader(InputStream in) {
            super(in);
        }
    }
}
