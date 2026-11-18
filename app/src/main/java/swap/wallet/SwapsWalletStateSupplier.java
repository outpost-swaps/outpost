package swap.wallet;

import com.samourai.wallet.client.indexHandler.IIndexHandler;
import com.samourai.wallet.client.indexHandler.MemoryIndexHandler;
import com.samourai.wallet.hd.AddressType;
import com.samourai.wallet.hd.Chain;
import com.samourai.whirlpool.client.wallet.beans.WhirlpoolAccount;
import com.samourai.whirlpool.client.wallet.data.walletState.WalletStateSupplier;

import java.util.LinkedHashMap;
import java.util.Map;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * In-memory WalletStateSupplier. Real receive/change indices are pulled from
 * Dojo on startup via SwapsWallet.syncIndicesFromDojo(); this only needs to
 * hold them for the lifetime of the process, not persist across restarts.
 */
public class SwapsWalletStateSupplier implements WalletStateSupplier {
    private final IIndexHandler indexHandlerExternal = new MemoryIndexHandler();
    private final Map<String, IIndexHandler> indexHandlerWallets = new LinkedHashMap<>();
    private final AtomicBoolean initialized = new AtomicBoolean(false);

    @Override
    public synchronized IIndexHandler getIndexHandlerWallet(WhirlpoolAccount account, AddressType addressType, Chain chain) {
        String key = account.name() + "_" + addressType.getPurpose() + "_" + chain.getIndex();
        return indexHandlerWallets.computeIfAbsent(key, k -> new MemoryIndexHandler());
    }

    @Override
    public IIndexHandler getIndexHandlerExternal() {
        return indexHandlerExternal;
    }

    @Override
    public boolean isInitialized() {
        return initialized.get();
    }

    @Override
    public void setInitialized(boolean initialized) {
        this.initialized.set(initialized);
    }

    @Override
    public void load() {
        // no-op: nothing to load, indices are synced from Dojo instead
    }

    @Override
    public boolean persist(boolean force) {
        // no-op: not persisted to disk; re-synced from Dojo on next startup
        return true;
    }
}
