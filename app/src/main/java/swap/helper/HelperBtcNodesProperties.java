package swap.helper;

import org.json.JSONArray;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import swap.lib.AppSwap;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileWriter;
import java.io.IOException;
import java.lang.invoke.MethodHandles;
import java.util.HashMap;
import java.util.Properties;

public class HelperBtcNodesProperties {
    private final Logger log = LoggerFactory.getLogger(MethodHandles.lookup().lookupClass());

    private static HelperBtcNodesProperties instance = null;

    public static HelperBtcNodesProperties getInstance() {
        if (instance == null)
            instance = new HelperBtcNodesProperties();
        return instance;
    }

    private static final String KEY_CUSTOM_NODES = "nodes";
    private HashMap<String, String> customNodes = new HashMap<>();

    public HelperBtcNodesProperties() {
        Properties appProperties = new Properties();
        try {
            if (hasPropertiesFile()) {
                loadProperties(appProperties, getPropertiesFile());
                String nodesArrayString = appProperties.getProperty(KEY_CUSTOM_NODES, "[]");
                JSONArray nodesJsonArray = new JSONArray(nodesArrayString);
                for (int i = 0; i < nodesJsonArray.length(); i++) {
                    String node = nodesJsonArray.getString(i);
                    customNodes.put(node, "");
                }
            } else {
                if (!AppSwap.getSwapRootDir().exists()) AppSwap.getSwapRootDir().mkdirs();
            }

            storeProperties(appProperties, getPropertiesFile(), "store custom-btc-nodes.properties");
        } catch (IOException e) {
            log.error("Failed to load props", e);
        }
    }

    public File getPropertiesFile() {
        return new File(AppSwap.getSwapRootDir(), "custom-btc-nodes.properties");
    }

    public boolean hasPropertiesFile() {
        return getPropertiesFile().exists();
    }

    public void setProperty(String key, String value) throws IOException {
        Properties props = new Properties();
        loadProperties(props, getPropertiesFile());
        props.setProperty(key, value);
        storeProperties(props, getPropertiesFile(), "store custom-btc-nodes.properties");
    }

    public String getProperty(String key) {
        Properties props = new Properties();
        try {
            loadProperties(props, getPropertiesFile());
        } catch (IOException e) {
            return null;
        }
        return props.getProperty(key);
    }

    public HashMap<String, String> getCustomNodes() {
        return customNodes;
    }

    public void addCustomNode(String btcNodeAddress) {
        customNodes.put(btcNodeAddress, "");
        save();
    }

    public void removeCustomNode(String btcNodeAddress) {
        customNodes.remove(btcNodeAddress);
        save();
    }

    private void save() {
        try {
            JSONArray jsonArray = new JSONArray();
            for (String nodeAddress : customNodes.keySet()) {
                jsonArray.put(nodeAddress);
            }
            setProperty(KEY_CUSTOM_NODES, jsonArray.toString());
        } catch (IOException e) {
            throw new RuntimeException(e);
        }
    }

    private static void loadProperties(Properties properties, File file) throws IOException {
        try (java.io.FileInputStream input = new java.io.FileInputStream(file)) {
            properties.load(input);
        }
    }

    private static void storeProperties(Properties properties, File file, String comment) throws IOException {
        try (java.io.FileWriter writer = new java.io.FileWriter(file)) {
            properties.store(writer, comment);
        }
    }
}
