package swap.mixing;

import com.google.common.eventbus.Subscribe;
import com.samourai.stomp.client.IStompClientService;
import com.samourai.tor.client.TorClientService;
import com.samourai.wallet.hd.HD_Wallet;
import com.samourai.whirlpool.client.event.MixFailEvent;
import com.samourai.whirlpool.client.event.MixProgressEvent;
import com.samourai.whirlpool.client.event.MixSuccessEvent;
import com.samourai.whirlpool.client.event.UtxoChangesEvent;
import com.samourai.whirlpool.client.exception.NotifiableException;
import com.samourai.whirlpool.client.tx0.Tx0;
import com.samourai.whirlpool.client.tx0.Tx0Config;
import com.samourai.whirlpool.client.tx0.Tx0Preview;
import com.samourai.whirlpool.client.tx0.Tx0Previews;
import com.samourai.whirlpool.client.wallet.WhirlpoolEventService;
import com.samourai.whirlpool.client.wallet.WhirlpoolWallet;
import com.samourai.whirlpool.client.wallet.WhirlpoolWalletConfig;
import com.samourai.whirlpool.client.wallet.beans.Tx0FeeTarget;
import com.samourai.whirlpool.client.wallet.beans.WhirlpoolAccount;
import com.samourai.whirlpool.client.wallet.beans.WhirlpoolServer;
import com.samourai.whirlpool.client.wallet.beans.WhirlpoolUtxo;
import com.samourai.whirlpool.client.wallet.data.dataSource.DataSourceFactory;
import com.samourai.whirlpool.client.wallet.data.dataSource.DojoDataSourceFactory;
import com.samourai.whirlpool.client.whirlpool.ServerApi;
import com.samourai.whirlpool.client.whirlpool.beans.Pool;
import com.samourai.whirlpool.protocol.WhirlpoolProtocol;
import com.sparrowwallet.nightjar.stomp.JavaStompClientService;
import com.sparrowwallet.nightjar.tor.WhirlpoolTorClientService;
import org.bitcoinj.core.NetworkParameters;
import org.bitcoinj.params.MainNetParams;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.lang.invoke.MethodHandles;
import java.util.Collection;
import java.util.Collections;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Optional post-swap Whirlpool mixing, backed by darkjar's WhirlpoolWallet.
 *
 * Decoupling invariant (see swaps-handoff/tasks/wp-reintegration.md §11): this is the
 * ONLY class in the app allowed to import com.samourai.whirlpool.client.* directly. The
 * swap core / SwapsWallet must never depend on this class compiling or succeeding — a
 * coordinator outage, disabled toggle, or construction failure must leave BTC sitting
 * unmixed in the ASB account with the swap otherwise unaffected. All public methods here
 * are therefore safe to call from a try/catch that just logs and moves on.
 *
 * Routes each HttpUsage (BACKEND/COORDINATOR_REST/COORDINATOR_WEBSOCKET/
 * COORDINATOR_REGISTER_OUTPUT) through its own Tor instance (GUISwap.startWhirlpoolTorClients
 * + PerUsageHttpClientService) so they aren't correlatable via a shared connection —
 * separate Tor daemons, not just separate SOCKS auth (Socks4Proxy here has none).
 */
public class MixingService {
    private static final Logger log = LoggerFactory.getLogger(MethodHandles.lookup().lookupClass());
    private static MixingService instance;

    private final HD_Wallet hdWallet;
    private final NetworkParameters params;
    private final String dojoUrl;
    private final String dojoApiKey;

    private WhirlpoolWallet whirlpoolWallet;
    private PerUsageHttpClientService httpClientService;
    private final AtomicBoolean available = new AtomicBoolean(false);
    private final AtomicBoolean starting = new AtomicBoolean(false);
    private final AtomicBoolean stopRequested = new AtomicBoolean(false);
    private final java.util.concurrent.ExecutorService autoTx0Executor =
            java.util.concurrent.Executors.newSingleThreadExecutor(r -> new Thread(r, "mixing-auto-tx0"));

    private MixingService(HD_Wallet hdWallet, NetworkParameters params, String dojoUrl, String dojoApiKey) {
        this.hdWallet = hdWallet;
        this.params = params;
        this.dojoUrl = dojoUrl;
        this.dojoApiKey = dojoApiKey;
    }

    public static MixingService getInstance() {
        return instance;
    }

    /**
     * Starts mixing if it isn't already running or already starting — safe to call
     * repeatedly (e.g. a user rapidly toggling the settings checkbox) without spawning
     * duplicate WhirlpoolWallet/Tor instances that would never get cleaned up. Kicks off
     * the actual (blocking) startup on a background thread and returns immediately.
     */
    public static synchronized void enable(HD_Wallet hdWallet, NetworkParameters params, String dojoUrl, String dojoApiKey) {
        if (instance != null && (instance.isAvailable() || instance.starting.get())) {
            return; // already running or already starting - don't spin up a duplicate
        }
        MixingService mixingService = new MixingService(hdWallet, params, dojoUrl, dojoApiKey);
        mixingService.starting.set(true);
        instance = mixingService;
        swap.gui.GUISwap.executorService.submit(mixingService::start0);
    }

    /** Stops mixing (if running/starting) — safe to call even if never started. */
    public static synchronized void disable() {
        if (instance != null) {
            instance.stop();
        }
    }

    /**
     * Opens and starts the WhirlpoolWallet. Does blocking network I/O (Dojo + coordinator
     * connectivity checks). Never throws: on any failure, mixing is simply unavailable for
     * this session. If stop() was called while this was still opening, tears back down
     * instead of marking available — closes the race between a fast disable-after-enable
     * and this method finishing its (slow) network setup.
     */
    private void start0() {
        // Built entirely on a local reference, not the `whirlpoolWallet` field - if this
        // sequence used the field instead, a concurrent doStop() (fast disable-after-enable)
        // could null it out or close() it mid-sequence while open()/start() are still
        // running on it from this thread, which is undefined behavior on a
        // non-thread-safe darkjar object, not just a clean NPE.
        WhirlpoolWallet ww = null;
        try {
            swap.gui.GUISwap.startWhirlpoolTorClients();
            ww = open();
            ww.open();
            WhirlpoolEventService.getInstance().register(this);
            ww.start();

            synchronized (this) {
                if (stopRequested.get()) {
                    WhirlpoolEventService.getInstance().unregister(this);
                    ww.close();
                    return;
                }
                this.whirlpoolWallet = ww;
                available.set(true);
            }
            log.info("MixingService started");
        } catch (Exception e) {
            available.set(false);
            log.warn("Whirlpool mixing unavailable: {}", e.getMessage());
            if (ww != null) {
                try {
                    ww.close();
                } catch (Exception closeException) {
                    log.warn("Error closing partially-started WhirlpoolWallet: {}", closeException.getMessage());
                }
            }
        } finally {
            starting.set(false);
        }
    }

    private synchronized void stop() {
        stopRequested.set(true);
        doStop();
    }

    private synchronized void doStop() {
        if (whirlpoolWallet != null) {
            try {
                WhirlpoolEventService.getInstance().unregister(this);
                whirlpoolWallet.close();
            } catch (Exception e) {
                log.warn("Error stopping MixingService: {}", e.getMessage());
            }
            whirlpoolWallet = null;
        }
        if (httpClientService != null) {
            httpClientService.shutdown();
            httpClientService = null;
        }
        autoTx0Executor.shutdownNow();
        available.set(false);
    }

    public boolean isAvailable() {
        return available.get();
    }

    private WhirlpoolWallet open() throws Exception {
        // One Tor circuit per HttpUsage (BACKEND/COORDINATOR_REST/COORDINATOR_WEBSOCKET/
        // COORDINATOR_REGISTER_OUTPUT) so they aren't correlatable via a shared connection.
        this.httpClientService = new PerUsageHttpClientService(swap.gui.GUISwap.getWhirlpoolTorClients());
        IStompClientService stompClientService = new JavaStompClientService(
                httpClientService.getJavaHttpClientService(com.samourai.http.client.HttpUsage.COORDINATOR_WEBSOCKET));
        TorClientService torClientService = new WhirlpoolTorClientService();

        WhirlpoolServer server = MainNetParams.get().equals(params) ? WhirlpoolServer.MAINNET : WhirlpoolServer.TESTNET;
        ServerApi serverApi = new ServerApi(server.getServerUrl(true), server.getServerUrlOutputReg(), httpClientService);

        DataSourceFactory dataSourceFactory = new DojoDataSourceFactory(dojoUrl, dojoApiKey, null);
        WhirlpoolWalletConfig config = new WhirlpoolWalletConfig(
                dataSourceFactory, httpClientService, stompClientService, torClientService, serverApi, params, false);
        // The coordinator only accepts partner ids it has registered: a live check on 2026-11-14 answered
        // "Partner not found" for outpostswaps, and accepted only Ashigaru's own
        // id. Until the coordinator's operator registers one for Outpost, use Ashigaru's. This credits
        // Ashigaru's partner share, so it is to be settled with them.
        config.setPartner(com.samourai.whirlpool.protocol.WhirlpoolProtocol.PARTNER_ID_ASHIGARU);
        // Tx0 itself is triggered explicitly (onUtxoChanges below), not by darkjar's own
        // autoTx0 orchestrator — but autoMix stays on, so premix outputs produced by that
        // Tx0 get picked up and actually mixed by darkjar's existing background loop
        // instead of sitting in PREMIX forever.
        config.setAutoMix(true);
        config.setAutoTx0PoolId(null);
        config.setScode(swap.helper.HelperProperties.getScode());

        return new WhirlpoolWallet(config, hdWallet);
    }

    // ================== Mixing operations ==================

    public Collection<Pool> getPools() {
        if (!isAvailable()) return Collections.emptyList();
        return whirlpoolWallet.getPoolSupplier().getPools();
    }

    public Collection<WhirlpoolUtxo> getMixableUtxos(WhirlpoolAccount account) {
        if (!isAvailable()) return Collections.emptyList();
        return whirlpoolWallet.getUtxoSupplier().findUtxos(account);
    }

    public Tx0Previews previewTx0(Collection<WhirlpoolUtxo> utxos, Tx0FeeTarget tx0FeeTarget, Tx0FeeTarget mixFeeTarget) throws Exception {
        requireAvailable();
        Tx0Config tx0Config = whirlpoolWallet.getTx0Config(tx0FeeTarget, mixFeeTarget);
        return whirlpoolWallet.tx0Previews(utxos, tx0Config);
    }

    /** Builds, signs AND broadcasts a Tx0 (darkjar pushes it internally). */
    public Tx0 tx0(Collection<WhirlpoolUtxo> utxos, Pool pool, Tx0FeeTarget tx0FeeTarget, Tx0FeeTarget mixFeeTarget) throws Exception {
        requireAvailable();
        Tx0Config tx0Config = whirlpoolWallet.getTx0Config(tx0FeeTarget, mixFeeTarget);
        return whirlpoolWallet.tx0(utxos, pool, tx0Config);
    }

    public void mixQueue(WhirlpoolUtxo utxo) throws NotifiableException {
        if (!isAvailable()) throw new NotifiableException("Mixing not available");
        whirlpoolWallet.mixQueue(utxo);
    }

    public void mixStop(WhirlpoolUtxo utxo) throws NotifiableException {
        if (!isAvailable()) return;
        whirlpoolWallet.mixStop(utxo);
    }

    private void requireAvailable() throws NotifiableException {
        if (!isAvailable()) {
            throw new NotifiableException("Mixing not available");
        }
    }

    // ================== Event bridge ==================
    // Bridged onto darkjar's own Guava EventBus (WhirlpoolEventService). Kept as plain
    // logging for now; MainController/UI hookup is Phase C5.

    @Subscribe
    public void onMixSuccess(MixSuccessEvent event) {
        log.info("Mix success: txid={}", event.getReceiveUtxo().getHash());
    }

    @Subscribe
    public void onMixFail(MixFailEvent event) {
        log.warn("Mix failed: reason={}, error={}", event.getMixFailReason(), event.getError());
    }

    @Subscribe
    public void onMixProgress(MixProgressEvent event) {
        log.debug("Mix progress: {}", event.getMixProgress());
    }

    // ================== Auto-Tx0 trigger ==================
    // Event-driven off darkjar's own utxo-change detection (fired whenever the periodic
    // Dojo refresh sees new/removed utxos) rather than a symmetric "swap completed" hook
    // on the ASB side — the JNI AsbListener surface only exposes periodic balance data,
    // not a one-shot "BTC redeemed" event, so this is the actual signal available.
    // Best-effort and fail-soft: any failure here must never affect the swap that funded
    // the deposit in the first place.
    //
    // Watches WhirlpoolAccount.ASB, not SWAPS_DEPOSIT: verified directly against the Rust
    // source (xmr-btc-swap/swap/src/bin/asb.rs -> init_bitcoin_wallet() ->
    // Wallet::new_samourai_asb() -> Bip84SamouraiAsb, account 2147483641). That's the ASB's
    // one and only Bitcoin wallet - everything it locks, redeems, and holds, including BTC
    // redeemed from completed swaps, lives there. SWAPS_DEPOSIT (2147483643) is a different
    // account entirely: it's the swap *client* (Bob/buyer role)'s own wallet
    // (xmr-btc-swap/swap/src/bin/swap.rs -> Wallet::new() -> Bip84SamouraiSwapDeposit) and
    // has nothing to do with ASB proceeds.

    @Subscribe
    public void onUtxoChanges(UtxoChangesEvent event) {
        if (!isAvailable()) return;
        Collection<WhirlpoolUtxo> newDeposits = event.getUtxoData().getUtxoChanges().getUtxosAdded().stream()
                .filter(utxo -> utxo.getAccount() == WhirlpoolAccount.ASB)
                .toList();
        if (newDeposits.isEmpty()) return;

        // Serialized (not a raw new Thread per event): overlapping utxo-change events that
        // reference the same not-yet-spent deposit would otherwise race two concurrent Tx0
        // attempts on the same coins. Submitting (not dropping) guarantees every batch is
        // still processed, just one at a time.
        autoTx0Executor.submit(() -> autoTx0(newDeposits));
    }

    private void autoTx0(Collection<WhirlpoolUtxo> utxos) {
        try {
            Tx0Previews previews = previewTx0(utxos, Tx0FeeTarget.BLOCKS_4, Tx0FeeTarget.BLOCKS_4);
            Tx0Preview best = selectPool(previews, swap.helper.HelperProperties.getPoolSize());
            if (best == null) {
                log.info("No eligible pool for ASB auto-Tx0 (balance too small?)");
                return;
            }
            Tx0 tx0 = tx0(utxos, best.getPool(), Tx0FeeTarget.BLOCKS_4, Tx0FeeTarget.BLOCKS_4);
            log.info("Auto-Tx0 broadcast: txid={}, pool={}", tx0.getTx().getHashAsString(), best.getPool().getPoolId());
        } catch (Exception e) {
            log.warn("Auto-Tx0 for ASB account failed (funds remain unmixed): {}", e.getMessage());
        }
    }

    /**
     * Prefers the pool matching the user's configured pool size (SettingsController,
     * e.g. "0.01btc", matched by denomination — pool IDs aren't a reliable match target).
     * Falls back to the largest eligible pool when unset/unmatched (maximizes anonymity
     * set for the given balance). Package-private + pure (no static state) for testing.
     */
    static Tx0Preview selectPool(Tx0Previews previews, String preferred) {
        if (preferred != null && !preferred.isEmpty()) {
            try {
                long preferredSats = Math.round(Double.parseDouble(preferred.toLowerCase().replace("btc", "").trim()) * 100_000_000L);
                for (Tx0Preview preview : previews.getTx0Previews()) {
                    if (preview.getPool().getDenomination() == preferredSats) {
                        return preview;
                    }
                }
            } catch (NumberFormatException ignored) {
                // fall through to largest-pool default
            }
        }
        return previews.getTx0Previews().stream()
                .max(java.util.Comparator.comparingLong(p -> p.getPool().getDenomination()))
                .orElse(null);
    }
}
