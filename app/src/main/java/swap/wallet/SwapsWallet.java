package swap.wallet;

import com.google.common.net.HostAndPort;
import com.samourai.http.client.HttpUsage;
import com.samourai.http.client.IHttpClient;
import com.samourai.wallet.SamouraiWalletConst;
import com.samourai.wallet.api.backend.BackendApi;
import com.samourai.wallet.api.backend.BackendOAuthApi;
import com.samourai.wallet.api.backend.beans.MultiAddrResponse;
import com.samourai.wallet.api.backend.beans.UnspentOutput;
import com.samourai.wallet.bip69.BIP69InputComparator;
import com.samourai.wallet.bip69.BIP69OutputComparator;
import com.samourai.wallet.client.BipWalletAndAddressType;
import com.samourai.wallet.hd.AddressType;
import com.samourai.wallet.hd.HD_Address;
import com.samourai.wallet.hd.HD_Wallet;
import com.samourai.wallet.send.MyTransactionOutPoint;
import com.samourai.wallet.send.SendFactoryGeneric;
import com.samourai.wallet.send.provider.SimpleUtxoKeyProvider;
import com.samourai.wallet.util.oauth.OAuthManager;
import com.samourai.wallet.util.oauth.OAuthManagerJava;
import com.samourai.whirlpool.client.wallet.beans.WhirlpoolAccount;
import com.samourai.whirlpool.client.wallet.data.wallet.WalletSupplierImpl;
import com.sparrowwallet.nightjar.http.JavaHttpClientService;
import org.berndpruenster.netlayer.tor.Tor;
import org.berndpruenster.netlayer.tor.TorCtlException;
import org.bitcoinj.core.Address;
import org.bitcoinj.core.Coin;
import org.bitcoinj.core.ECKey;
import org.bitcoinj.core.NetworkParameters;
import org.bitcoinj.core.Transaction;
import org.bitcoinj.core.TransactionInput;
import org.bitcoinj.core.TransactionOutput;
import org.bitcoinj.script.Script;
import org.bouncycastle.util.encoders.Hex;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import swap.gui.GUISwap;
import swap.gui.controller.MainController;
import swap.gui.controller.pages.SwapsController;
import swap.helper.HelperAddress;
import swap.model.AsbBtcBalanceData;
import swap.model.LogType;

import java.lang.invoke.MethodHandles;
import java.util.ArrayList;
import java.util.Collection;
import java.util.Collections;
import java.util.HashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Wallet service replacing ClientWhirlpool + ServiceWhirlpool
 * Delegates HD key derivation to darkjar's HD_Wallet + WalletSupplierImpl.
 * Balance queries go through darkjar's BackendApi (Dojo).
 *
 * TODO: WP integration (see MixingService, once built)
 */
public class SwapsWallet {
    private static final Logger log = LoggerFactory.getLogger(MethodHandles.lookup().lookupClass());
    private static SwapsWallet instance;

    private final AtomicBoolean ready = new AtomicBoolean(false);
    private final NetworkParameters params;
    private final HD_Wallet hdWallet;
    private final WalletSupplierImpl walletSupplier;
    private final BackendApi backendApi;

    /**
     * Next-block fee estimator from the paired Dojo: confirmation
     * probabilities ("0.999","0.99","0.9","0.5","0.2","0.1") → sat/vB.
     * Used for withdraw-fee defaults instead of hard-coded sat/vB prompts.
     */
    public Map<String, Integer> fetchFeeEstimates() throws Exception {
        return backendApi.fetchFeesEstimator();
    }
    private final Set<String> pendingIndexSync = ConcurrentHashMap.newKeySet();
    private final AtomicBoolean indexSyncRunning = new AtomicBoolean(false);
    private final AtomicBoolean indexSyncFailureReported = new AtomicBoolean(false);
    private final DojoOutageNotice dojoOutage = new DojoOutageNotice();
    private volatile String dojoStatus = "not checked";
    private volatile boolean dojoReachable;

    // ==================  Lifecycle ==================

    public SwapsWallet(List<String> mnemonic, String bip39Passphrase,
                       NetworkParameters params, String dojoUrl, String dojoApiKey) {
        this.params = params;
        uiLog(LogType.INFO, "Deriving wallet accounts...");

        // The wallet root derives the swap-specific BIP84 accounts. Samourai's standard
        // DEPOSIT account also has legacy and wrapped-SegWit address types in darkjar.
        // mSeed (entropy) is null - mnemonic + passphrase are sufficient for derivation.
        this.hdWallet = new HD_Wallet(84, mnemonic, params, null, bip39Passphrase);

        // Register all accounts (DEPOSIT, PREMIX, POSTMIX, BADBANK, SWAPS_DEPOSIT, SWAPS_REFUNDS, ASB)
        this.walletSupplier = new WalletSupplierImpl(hdWallet, new SwapsWalletStateSupplier());

        // Dojo backend for utxo queries and address index sync
        BackendApi configuredBackend = null;
        if (dojoUrl != null && !dojoUrl.isEmpty()) {
            try {
                IHttpClient httpClient = computeDojoHttpClient();
                if (dojoApiKey != null && !dojoApiKey.isEmpty()) {
                    OAuthManager oAuthManager = new OAuthManagerJava(dojoApiKey, new BackendOAuthApi(httpClient, dojoUrl));
                    configuredBackend = new BackendApi(httpClient, dojoUrl, oAuthManager);
                } else {
                    configuredBackend = new BackendApi(httpClient, dojoUrl);
                }
                log.info("SwapsWallet: Dojo backend configured at {}", dojoUrl);
            } catch (IllegalStateException e) {
                log.error("Dojo backend disabled because Tor is unavailable", e);
                uiLog(LogType.ERROR, e.getMessage());
            }
            if (configuredBackend != null) {
                for (BipWalletAndAddressType wallet : walletSupplier.getWallets()) {
                    pendingIndexSync.add(wallet.getPub());
                }
                this.backendApi = configuredBackend;
                dojoStatus = "checking over Tor";
                updateDojoIndicator(dojoStatus, false, true);
                uiLog(LogType.INFO, "Dojo configured; syncing address indices over Tor in background...");
                GUISwap.executorService.submit(this::syncIndicesFromDojo);
            } else {
                this.backendApi = null;
                dojoStatus = "unavailable: Tor proxy unavailable";
                updateDojoIndicator(dojoStatus, false, false);
            }
        } else {
            this.backendApi = null;
            dojoStatus = "not configured";
            updateDojoIndicator(dojoStatus, false, false);
            log.warn("SwapsWallet: No Dojo URL — backend disabled. Balances and withdraw will not work. (dojoUrl was: '{}')", dojoUrl);
            uiLog(LogType.ERROR, "No Dojo URL configured; wallet balances and withdrawals are unavailable.");
        }

        instance = this;
        ready.set(true);
        log.info("Initialized wallet with {} accounts", walletSupplier.getWallets().size());
    }

    /**
     * java.net.http.HttpClient (the JDK's built-in client) does not actually support SOCKS
     * proxies through its public ProxySelector API — only HTTP CONNECT-style forward
     * proxies. Routing it at Tor's SOCKS port silently corrupts every response (this was a
     * real, previously-undetected bug: every Dojo call failed and every balance showed
     * zero). darkjar's own Jetty-based JavaHttpClientService (Socks4Proxy) is the
     * proven-correct transport for Tor and is what the rest of the app's Whirlpool traffic
     * already uses — reuse it here instead of a second, broken HTTP stack.
     */
    private static IHttpClient computeDojoHttpClient() {
        HostAndPort torProxy;
        try {
            Tor tor = Tor.getDefault();
            if (tor == null) {
                throw new IllegalStateException("Tor is not running; refusing to connect to Dojo without Tor.");
            }
            int proxyPort = tor.getProxy("127.0.0.1").getPort();
            torProxy = HostAndPort.fromParts("127.0.0.1", proxyPort);
        } catch (TorCtlException | RuntimeException e) {
            throw new IllegalStateException("Tor proxy unavailable; refusing clearnet Dojo access.", e);
        }
        return new JavaHttpClientService(torProxy).getHttpClient(HttpUsage.BACKEND);
    }

    private static void uiLog(LogType type, String message) {
        MainController controller = MainController.getInstance();
        if (controller != null) controller.printSwapLogLn(type, message, false);
        log.info(message);
    }

    private void updateDojoIndicator(String status, boolean reachable, boolean checking) {
        MainController controller = MainController.getInstance();
        if (controller != null) controller.setDojoStatus(status, reachable, checking);
    }

    public static SwapsWallet getInstance() {
        return instance;
    }

    public boolean isReady() {
        return ready.get();
    }

    public String getDojoStatus() {
        return dojoStatus;
    }

    public boolean isDojoReachable() {
        return dojoReachable;
    }

    public void shutdown() {
        ready.set(false);
        instance = null;
    }

    public HD_Wallet getHdWallet() {
        return hdWallet;
    }

    public WalletSupplierImpl getWalletSupplier() {
        return walletSupplier;
    }


    // ==================  Address Index Sync ==================

    /**
     * Sync address indices from Dojo /v2/multiaddr on startup.
     * Prevents address reuse after app restart by setting each account's
     * receive and change index to the values Dojo has seen.
     */
    private void syncIndicesFromDojo() {
        if (backendApi == null || pendingIndexSync.isEmpty() || !indexSyncRunning.compareAndSet(false, true)) return;
        try {
            List<BipWalletAndAddressType> pending = new ArrayList<>();
            for (BipWalletAndAddressType wallet : walletSupplier.getWallets()) {
                if (pendingIndexSync.contains(wallet.getPub())) pending.add(wallet);
            }
            if (pending.isEmpty()) return;

            if (!indexSyncFailureReported.get()) {
                uiLog(LogType.INFO, "Requesting Dojo address indices for " + pending.size()
                        + " account/address-type pairs through Tor...");
            }
            try {
                String[] pubs = pending.stream().map(BipWalletAndAddressType::getPub).toArray(String[]::new);
                Map<String, MultiAddrResponse.Address> addresses = backendApi.fetchAddresses(pubs);
                dojoStatus = "reachable over Tor";
                dojoReachable = true;
                updateDojoIndicator(dojoStatus, true, false);
                int synced = 0;
                for (BipWalletAndAddressType wallet : pending) {
                    MultiAddrResponse.Address address = addresses.get(wallet.getPub());
                    if (address == null) continue;
                    wallet.getIndexHandler().set(address.account_index, false);
                    wallet.getIndexChangeHandler().set(address.change_index, false);
                    pendingIndexSync.remove(wallet.getPub());
                    synced++;
                    log.debug("Synced indices for {}/{}: receive={}, change={}",
                            wallet.getAccount(), wallet.getAddressType(), address.account_index, address.change_index);
                }
                if (pendingIndexSync.isEmpty()) {
                    if (indexSyncFailureReported.getAndSet(false)) {
                        uiLog(LogType.INFO, "Dojo address-index sync recovered; all address types are synced.");
                    } else {
                        uiLog(LogType.INFO, "Dojo address-index sync complete for all address types.");
                    }
                } else if (indexSyncFailureReported.compareAndSet(false, true)) {
                    uiLog(LogType.WARN, "Dojo returned indices for " + synced + "/" + pending.size()
                            + " wallets; remaining address types will retry in the background.");
                }
            } catch (Exception e) {
                dojoStatus = "unreachable over Tor; retrying";
                dojoReachable = false;
                updateDojoIndicator(dojoStatus, false, false);
                log.warn("Tor-only Dojo address-index sync failed; will retry during balance refresh", e);
                if (indexSyncFailureReported.compareAndSet(false, true)) {
                    uiLog(LogType.WARN, "Dojo index sync failed over Tor ("
                            + dojoFailureReason(e)
                            + "). Retrying in the background; no clearnet fallback will be used.");
                }
            }
        } finally {
            indexSyncRunning.set(false);
        }
    }


    // ==================  Public API: Addresses ==================

    public String getReceiveAddress(WhirlpoolAccount account) {
        BipWalletAndAddressType bw = walletSupplier.getWallet(account, AddressType.SEGWIT_NATIVE);
        if (bw == null) return "";
        return bw.getNextAddress().getAddressString(AddressType.SEGWIT_NATIVE);
    }

    public String getChangeAddress(WhirlpoolAccount account) {
        BipWalletAndAddressType bw = walletSupplier.getWallet(account, AddressType.SEGWIT_NATIVE);
        if (bw == null) return "";
        return bw.getNextChangeAddress().getAddressString(AddressType.SEGWIT_NATIVE);
    }

    public String getZpub(WhirlpoolAccount account) {
        return getZpub(account, AddressType.SEGWIT_NATIVE);
    }

    public String getZpub(WhirlpoolAccount account, AddressType addressType) {
        BipWalletAndAddressType bw = walletSupplier.getWallet(account, addressType);
        if (bw == null) return "";
        return bw.getPub();
    }

    public int getNextReceiveIndex(WhirlpoolAccount account, AddressType addressType) {
        BipWalletAndAddressType bw = walletSupplier.getWallet(account, addressType);
        return bw == null ? 0 : bw.getIndexHandler().get();
    }

    public int getNextChangeIndex(WhirlpoolAccount account, AddressType addressType) {
        BipWalletAndAddressType bw = walletSupplier.getWallet(account, addressType);
        return bw == null ? 0 : bw.getIndexChangeHandler().get();
    }

    public List<String> getAccountPubs(WhirlpoolAccount account) {
        List<String> pubs = new ArrayList<>();
        for (AddressType type : account.getAddressTypes()) {
            String pub = getZpub(account, type);
            if (!pub.isEmpty()) pubs.add(pub);
        }
        return pubs;
    }

    // ==================  Public API: Balance & UTXOS ==================

    public long getBalance(WhirlpoolAccount account) {
        return UnspentOutput.sumValue(getUtxos(account));
    }

    /** Always renders exactly 8 decimals (full sat precision) for log/UI messages. */
    public static String toBtcString(Coin coin) {
        return java.math.BigDecimal.valueOf(coin.getValue(), 8).toPlainString();
    }

    public Collection<UnspentOutput> getUtxos(WhirlpoolAccount account) {
        try {
            return queryUtxos(account);
        } catch (Exception e) {
            log.debug("Failed to fetch UTXOs from Dojo: {}", e.getMessage());
            return Collections.emptyList();
        }
    }

    /** Fetch account UTXOs, preserving failures for UI flows that must not show stale/zero balances. */
    public Collection<UnspentOutput> queryUtxos(WhirlpoolAccount account) throws Exception {
        if (backendApi == null) {
            throw new IllegalStateException("Dojo backend is not configured.");
        }

        String[] pubs = getAccountPubs(account).toArray(new String[0]);
        if (pubs.length == 0) throw new IllegalStateException("No address types registered for " + account);
        try {
            Map<String, UnspentOutput> distinct = new LinkedHashMap<>();
            for (UnspentOutput utxo : backendApi.fetchUtxos(pubs)) {
                distinct.put(utxo.tx_hash + ":" + utxo.tx_output_n, utxo);
            }
            markDojoReachable();
            return new ArrayList<>(distinct.values());
        } catch (Exception e) {
            markDojoUnreachable(e);
            throw e;
        }
    }

    private void markDojoReachable() {
        dojoReachable = true;
        dojoStatus = "reachable over Tor";
        updateDojoIndicator(dojoStatus, true, false);
        // Only log the recovery if a failure was previously announced, so the
        // log explains why balances were missing and that they are back.
        boolean outageAnnounced = dojoOutage.succeeded();
        boolean indexSyncFailureShown = indexSyncFailureReported.compareAndSet(true, false);
        if (outageAnnounced || indexSyncFailureShown) {
            uiLog(LogType.INFO, "Dojo reachable again over Tor; balances updated.");
        }
    }

    private void markDojoUnreachable(Exception error) {
        dojoReachable = false;
        dojoStatus = "unreachable over Tor; retrying";
        // The indicator shows every failure; the log only an outage that lasts (DojoOutageNotice).
        updateDojoIndicator(dojoStatus, false, false);
        if (dojoOutage.failed(System.currentTimeMillis())) {
            String message = isStoppedHttpClient(error)
                    ? "Dojo connection closed for over a minute; retrying automatically. Balances may be temporarily unavailable."
                    : "Couldn't reach Dojo over Tor for over a minute; retrying automatically. Balances may be temporarily unavailable.";
            uiLog(LogType.WARN, message);
        }
    }

    private boolean isStoppedHttpClient(Throwable error) {
        for (Throwable cause = error; cause != null; cause = cause.getCause()) {
            if (cause instanceof java.util.concurrent.RejectedExecutionException
                    && cause.getMessage() != null && cause.getMessage().contains("is stopped")) {
                return true;
            }
        }
        return false;
    }

    // ==================  GUI Balance Updates ==================

    private Coin lastAsbBtcBalance;

    public void startPeriodicBalanceUpdates() {
        GUISwap.scheduledExecutorService.scheduleAtFixedRate(() -> {
            try {
                syncIndicesFromDojo();
                updateBalancesForGui();
            } catch (Exception e) {
                log.debug("Balance update failed: {}", e.getMessage());
            }
        }, 0, 15, TimeUnit.SECONDS);
    }

    public void refreshAsbBalanceNow() {
        updateBalancesForGui();
    }

    private void updateBalancesForGui() {
        // Liquidity page only surfaces the ASB's own BTC balance; the
        // DEPOSIT/POSTMIX/BADBANK rows were removed, so skip their Dojo
        // queries entirely.
        String[] asbAccounts = {
                "SWAPS_ASB"
        };
        WhirlpoolAccount[] asbWallets = {
                WhirlpoolAccount.ASB
        };

        for (int i = 0; i < asbAccounts.length; i++) {
            try {
                Coin coinBalance = Coin.valueOf(UnspentOutput.sumValue(queryUtxos(asbWallets[i])));
                if (lastAsbBtcBalance == null || !lastAsbBtcBalance.equals(coinBalance)) {
                    lastAsbBtcBalance = coinBalance;
                    log.info("ASB BTC balance updated: {}", coinBalance.toFriendlyString());
                }
                if (MainController.asbListener != null) {
                    MainController.asbListener.onAsbBtcBalanceData(asbAccounts[i],
                            new AsbBtcBalanceData(coinBalance, ""));
                }
            } catch (Exception e) {
                if (MainController.asbListener != null) {
                    MainController.asbListener.onAsbBtcBalanceData(asbAccounts[i],
                            new AsbBtcBalanceData(Coin.ZERO, "Dojo unavailable over Tor; balance not current"));
                }
            }
        }
    }

    // ==================  Transaction Construction & Send ==================
    //
    // PRIVACY WARNING: Naive linear coin selection. Will merge UTXOs from the
    // same account into one transaction, linking them on-chain. Treat each
    // WhirlpoolAccount as its own privacy boundary; sweep small amounts
    // deliberately. Ported from ServiceWhirlpool.createTransactionFromAccount.

    /**
     * Convenience overload — auto-select UTXOs via linear coin selection.
     */
    public WithdrawPlan prepareWithdraw(Address destination, Coin amount, WhirlpoolAccount sourceWallet, long feeRate) throws Exception {
        return prepareWithdraw(destination, amount, sourceWallet, feeRate, null);
    }

    /**
     * Build and sign a withdrawal tx WITHOUT broadcasting. Caller can inspect
     * the WithdrawPlan (inputs, outputs, fee, raw hex) and decide whether to
     * call broadcastWithdraw().
     *
     * Pass amount = Coin.valueOf(Long.MAX_VALUE) to sweep.
     *
     * @param explicitUtxos If non-null and non-empty, use these UTXOs exactly
     *                      (no coin selection). The user is responsible for
     *                      ensuring sum(explicitUtxos) covers amount + fee.
     *                      If null/empty, auto-select linearly.
     */
    public WithdrawPlan prepareWithdraw(Address destination, Coin amount, WhirlpoolAccount sourceWallet, long feeRate,
                                        List<UnspentOutput> explicitUtxos) throws Exception {
        boolean useExplicit = explicitUtxos != null && !explicitUtxos.isEmpty();
        if (backendApi == null && !useExplicit) {
            throw new IllegalStateException("Dojo backend not configured — cannot fetch UTXOs.");
        }
        BipWalletAndAddressType changeWallet = walletSupplier.getWallet(sourceWallet, AddressType.SEGWIT_NATIVE);
        if (changeWallet == null) {
            throw new IllegalStateException("Source wallet not found: " + sourceWallet);
        }

        Collection<UnspentOutput> utxos = useExplicit ? explicitUtxos : queryUtxos(sourceWallet);
        if (utxos.isEmpty()) {
            throw new IllegalStateException("No UTXOs available in account " + sourceWallet);
        }
        // Explicit UTXOs come from the UI's checkbox selection, not a Dojo query scoped to
        // this account's zpub - verify each one actually derives to sourceWallet before
        // spending it. Never trust the caller not to have handed us a UTXO from a different
        // account (e.g. a stale table from a fast account-switch race): merging UTXOs across
        // accounts here would be exactly the on-chain linkage this wallet's account
        // separation exists to prevent.
        if (useExplicit) {
            for (UnspentOutput utxo : utxos) {
                AddressType addressType = addressTypeFor(utxo);
                BipWalletAndAddressType source = walletSupplier.getWallet(sourceWallet, addressType);
                if (source == null) {
                    throw new IllegalStateException("Account " + sourceWallet + " does not support " + addressType);
                }
                HD_Address expected = source.getAddressAt(utxo);
                String expectedAddress = expected != null ? expected.getAddressString(addressType) : null;
                if (expectedAddress == null || !expectedAddress.equals(utxo.addr)) {
                    throw new IllegalStateException("UTXO " + utxo.tx_hash + ":" + utxo.tx_output_n
                            + " (address " + utxo.addr + ") does not belong to account " + sourceWallet
                            + " — refusing to merge UTXOs across accounts.");
                }
            }
        }

        Coin sumUtxos = Coin.valueOf(UnspentOutput.sumValue(utxos));
        boolean deductFromDestinationUtxo = sumUtxos.equals(amount) || amount.equals(Coin.valueOf(Long.MAX_VALUE));
        long numOutputs = deductFromDestinationUtxo ? 1 : 2;

        // Coin selection: explicit (use all provided) or linear (greedy)
        HashMap<UnspentOutput, Coin> selectedUtxos = new HashMap<>();
        Coin sumInputs = Coin.ZERO;
        Coin fee = Coin.ZERO;

        if (useExplicit) {
            for (UnspentOutput utxo : utxos) {
                selectedUtxos.put(utxo, Coin.valueOf(utxo.value));
                sumInputs = sumInputs.add(Coin.valueOf(utxo.value));
            }
            fee = Coin.valueOf(estimateFee(selectedUtxos, numOutputs, destination, feeRate));
        } else {
            HashMap<String, UnspentOutput> seenTxs = new HashMap<>();
            for (UnspentOutput utxo : utxos) {
                Coin inputValue = Coin.valueOf(utxo.value);
                if (deductFromDestinationUtxo || seenTxs.get(utxo.tx_hash) == null) {
                    sumInputs = sumInputs.add(inputValue);
                    selectedUtxos.put(utxo, inputValue);
                    seenTxs.put(utxo.tx_hash, utxo);
                    fee = Coin.valueOf(estimateFee(selectedUtxos, numOutputs, destination, feeRate));
                    Coin needed = deductFromDestinationUtxo ? amount.subtract(fee) : amount.add(fee);
                    if (sumInputs.isGreaterThan(needed)) break;
                }
            }
        }
        if (!amount.equals(Coin.valueOf(Long.MAX_VALUE)) && sumInputs.isLessThan(amount.add(fee))) {
            throw new IllegalStateException("Insufficient funds: selected inputs " + toBtcString(sumInputs)
                    + " cannot cover " + toBtcString(amount) + " + fee " + toBtcString(fee)
                    + (useExplicit ? " — select more UTXOs or pick larger ones" : ""));
        }

        Coin actualAmount = deductFromDestinationUtxo ? sumInputs.subtract(fee) : amount;
        Coin change = sumInputs.subtract(actualAmount).subtract(fee);

        // Build tx
        Transaction tx = new Transaction(params);
        List<TransactionInput> inputs = new ArrayList<>();
        List<TransactionOutput> outputs = new ArrayList<>();
        SimpleUtxoKeyProvider keyProvider = new SimpleUtxoKeyProvider();
        List<UnspentOutput> selectedList = new ArrayList<>(selectedUtxos.keySet());

        for (UnspentOutput utxo : selectedList) {
            MyTransactionOutPoint outpoint = utxo.computeOutpoint(params);
            TransactionInput input = outpoint.computeSpendInput();
            input.setSequenceNumber(SamouraiWalletConst.RBF_SEQUENCE_VAL.longValue());
            inputs.add(input);

            AddressType addressType = addressTypeFor(utxo);
            BipWalletAndAddressType source = walletSupplier.getWallet(sourceWallet, addressType);
            if (source == null) {
                throw new IllegalStateException("Account " + sourceWallet + " does not support " + addressType);
            }
            HD_Address hdAddress = source.getAddressAt(utxo);
            if (hdAddress == null) {
                throw new IllegalStateException("Cannot derive key for UTXO " + utxo.tx_hash + ":" + utxo.tx_output_n + " (no path)");
            }
            ECKey key = hdAddress.getECKey();
            keyProvider.setKey(outpoint, key);
        }

        Address changeAddress = null;
        if (actualAmount.isGreaterThan(Coin.valueOf(546))) {
            outputs.add(new TransactionOutput(params, tx, actualAmount, destination));
        }
        if (!deductFromDestinationUtxo && change.isGreaterThan(Coin.valueOf(546))) {
            changeAddress = HelperAddress.getAddress(changeWallet.getNextChangeAddress()
                    .getAddressString(AddressType.SEGWIT_NATIVE), params);
            if (changeAddress != null) {
                outputs.add(new TransactionOutput(params, tx, change, changeAddress));
            }
        }

        inputs.sort(new BIP69InputComparator());
        for (TransactionInput input : inputs) tx.addInput(input);
        outputs.sort(new BIP69OutputComparator());
        for (TransactionOutput output : outputs) tx.addOutput(output);

        Transaction signedTx = SendFactoryGeneric.getInstance().signTransaction(tx, keyProvider);
        String rawHex = Hex.toHexString(signedTx.bitcoinSerialize());

        return new WithdrawPlan(sourceWallet, destination, changeAddress,
                selectedList, actualAmount, change, fee, feeRate,
                signedTx, rawHex);
    }

    private AddressType addressTypeFor(UnspentOutput utxo) {
        return AddressType.findByAddress(utxo.addr, params);
    }

    /**
     * Broadcast a previously-prepared signed tx via Dojo's pushTx.
     * darkjar's BackendApi.pushTx() is void (throws on failure), so the txid
     * is computed locally from the already-signed transaction.
     */
    public String broadcastWithdraw(WithdrawPlan plan) throws Exception {
        if (backendApi == null) {
            throw new IllegalStateException("Dojo backend not configured.");
        }
        log.info("Broadcasting withdraw tx: {} inputs, {} outputs, fee={} sat",
                plan.inputs.size(), (plan.changeAddress != null ? 2 : 1), plan.fee.value);
        backendApi.pushTx(plan.rawHex);
        return plan.signedTx.getHashAsString();
    }

    /**
     * Holds a prepared (signed but not broadcast) withdrawal so the UI can
     * preview the tx structure before the user commits.
     */
    public static class WithdrawPlan {
        public final WhirlpoolAccount sourceWallet;
        public final Address destination;
        public final Address changeAddress; // null if sweep
        public final List<UnspentOutput> inputs;
        public final Coin amount;
        public final Coin change;
        public final Coin fee;
        public final long feeRate;
        public final Transaction signedTx;
        public final String rawHex;

        WithdrawPlan(WhirlpoolAccount sourceWallet, Address destination, Address changeAddress,
                     List<UnspentOutput> inputs, Coin amount, Coin change, Coin fee, long feeRate,
                     Transaction signedTx, String rawHex) {
            this.sourceWallet = sourceWallet;
            this.destination = destination;
            this.changeAddress = changeAddress;
            this.inputs = inputs;
            this.amount = amount;
            this.change = change;
            this.fee = fee;
            this.feeRate = feeRate;
            this.signedTx = signedTx;
            this.rawHex = rawHex;
        }

        public int vbytes() {
            return signedTx.getVirtualTransactionSize();
        }

        public Coin totalIn() {
            return amount.add(change).add(fee);
        }
    }

    private long estimateFee(HashMap<UnspentOutput, Coin> inputs, long numOutputs, Address destination, long feeRate) {
        long numInputsP2pkh = 0, numInputsP2shP2wpkh = 0, numInputsP2wpkh = 0;
        for (UnspentOutput utxo : inputs.keySet()) {
            Script script = utxo.computeScript();
            if (script.isSentToAddress()) numInputsP2pkh++;
            else if (script.isPayToScriptHash()) numInputsP2shP2wpkh++;
            else if (script.isSentToP2WPKH()) numInputsP2wpkh++;
        }
        long numOutputsP2pkh = 0, numOutputsP2sh = 0;
        long numOutputsP2wpkh = numOutputs == 2 ? 1 : 0; // change is P2WPKH
        long numOutputsP2wsh = 0, numOutputsP2tr = 0;
        if (destination.isP2WPKHAddress()) numOutputsP2wpkh++;
        else if (destination.isP2SHAddress()) numOutputsP2sh++;
        else if (destination.isP2WSHAddress()) numOutputsP2wsh++;
        else numOutputsP2pkh++;

        long total = 11; // overhead
        total += numInputsP2pkh * 148L;
        total += numInputsP2shP2wpkh * 91L;
        total += numInputsP2wpkh * 68L;
        total += numOutputsP2pkh * 34L;
        total += numOutputsP2sh * 32L;
        total += numOutputsP2wpkh * 31L;
        total += numOutputsP2wsh * 43L;
        total += numOutputsP2tr * 43L;
        return total * feeRate;
    }

    // ==================  TODO: Whirlpool ==================
    // When WP client is restored, add mixing integration here.

    /** Human-readable cause for the UI log; the full stack stays in the file log. */
    private static String dojoFailureReason(Throwable error) {
        for (Throwable cause = error; cause != null; cause = cause.getCause()) {
            if (cause instanceof java.net.SocketTimeoutException) return "timed out";
            if (cause instanceof java.net.ConnectException) return "could not connect";
            if (cause instanceof java.net.UnknownHostException) return "host not found";
        }
        return "network error";
    }
}
