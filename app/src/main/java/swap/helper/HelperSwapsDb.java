package swap.helper;

import org.bitcoinj.core.Sha256Hash;
import org.json.JSONObject;
import swap.model.SwapCoin;

public class HelperSwapsDb {
    private static HelperSwapsDb instance = null;
    public static HelperSwapsDb getInstance() {
        if(instance == null) {
            instance = new HelperSwapsDb();
        }

        return instance;
    }

    public Sha256Hash getLockTxid(SwapCoin swapCoin, String swapId) {
        JSONObject swapJson = getSwap(swapId);
        return Sha256Hash.wrap(swapJson.getString(swapCoin.name().toLowerCase() + "_lock_txid"));
    }

    public void setLockTxid(SwapCoin swapCoin, String swapId, String txid) {
        JSONObject swapJson = getSwap(swapId);
        swapJson.put(swapCoin.name().toLowerCase() + "_lock_txid", txid);
        upsertSwap(swapId, swapJson);
    }

    public long getSwapsAccount(String swapId) {
        JSONObject swapDb = HelperRawJsonDb.getInstance().getDb("swaps");
        JSONObject swapJson = swapDb.optJSONObject("swap_" + swapId);
        if(swapJson == null || !swapJson.has("swaps_account") || swapJson.isNull("swaps_account")) {
            // Fail closed: a live swap must never resume with a silently-defaulted
            // account, or refund/redeem BTC would be derived from the wrong account.
            throw new IllegalStateException(
                    "Missing swaps_account metadata for swap " + swapId + "; refusing to resume with a default account.");
        }

        return swapJson.getLong("swaps_account");
    }

    public void setSwapsAccount(String swapId, long swapsAccount) {
        JSONObject swapJson = getSwap(swapId);
        swapJson.put("swaps_account", swapsAccount);

        upsertSwap(swapId, swapJson);
    }

    private JSONObject getSwap(String swapId) {
        JSONObject swapDb = HelperRawJsonDb.getInstance().getDb("swaps");
        JSONObject swapJson = swapDb.optJSONObject("swap_" + swapId);
        if(swapJson == null) {
            return new JSONObject();
        }

        return swapJson;
    }

    private void upsertSwap(String swapId, JSONObject swapJson) {
        JSONObject swapDb = HelperRawJsonDb.getInstance().getDb("swaps");
        swapDb.put("swap_" + swapId, swapJson);
        HelperRawJsonDb.getInstance().updateDb("swaps", swapDb);
    }
}
