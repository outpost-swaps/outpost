package swap.model;

import org.json.JSONObject;

import java.security.SecureRandom;

public class SwapsPairing {
    private static SwapsPairing instance = null;
    private final String encryptedSeed;
    private final boolean hasPassphrase;
    private final String network; // "MAINNET" or "TESTNET"
    private final String apiKey;
    private String dojoUrl = null;
    private String dojoApiKey = null;

    /**
     * Parses Samourai pairing payload JSON.
     * Expected format:
     * {
     *   "pairing": { "network": "mainnet"|"testnet", "mnemonic": "...", "passphrase": true|false },
     *   "dojo": { "url": "...", "apikey": "..." }   // optional
     * }
     */
    public SwapsPairing(String pairingJson) throws Exception {
        instance = this;
        JSONObject root = new JSONObject(pairingJson);
        JSONObject pairing = root.getJSONObject("pairing");

        this.encryptedSeed = pairing.getString("mnemonic");
        this.hasPassphrase = pairing.optBoolean("passphrase", false);
        this.network = pairing.getString("network").toUpperCase();
        this.apiKey = generateApiKey();

        // Dojo block is nested: { "dojo": { "pairing": { "url": ..., "apikey": ... } } }
        // (matches Samourai's PairingDojo.toJson / parse format)
        if (root.has("dojo")) {
            JSONObject dojo = root.getJSONObject("dojo");
            if (dojo.has("pairing")) {
                JSONObject dojoPairing = dojo.getJSONObject("pairing");
                this.dojoUrl = dojoPairing.optString("url", null);
                this.dojoApiKey = dojoPairing.optString("apikey", null);
            } else {
                // Fallback: some payloads put url/apikey at the dojo top level
                this.dojoUrl = dojo.optString("url", null);
                this.dojoApiKey = dojo.optString("apikey", null);
            }
        }
    }

    public static String generateApiKey() {
        int APIKEY_LENGTH = 64;
        SecureRandom random = new SecureRandom();
        StringBuilder sb = new StringBuilder(APIKEY_LENGTH);
        String chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        for (int i = 0; i < APIKEY_LENGTH; i++) {
            sb.append(chars.charAt(random.nextInt(chars.length())));
        }
        return sb.toString();
    }

    public String getEncryptedSeed() {
        return encryptedSeed;
    }

    public String getNetwork() {
        return network;
    }

    public boolean hasPassphrase() {
        return hasPassphrase;
    }

    public String getApiKey() {
        return apiKey;
    }

    public String getDojoApiKey() {
        return dojoApiKey;
    }

    public String getDojoUrl() {
        return dojoUrl;
    }

    public static SwapsPairing getInstance() {
        return instance;
    }
}
