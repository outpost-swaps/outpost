package swap.client;

import org.berndpruenster.netlayer.tor.Tor;
import org.bitcoinj.core.NetworkParameters;
import org.json.JSONObject;
import swap.helper.HelperAddress;
import swap.helper.HelperMoneroWitnesses;
import swap.helper.HelperRawJsonDb;
import swap.helper.HelperSwapsDb;
import swap.lib.App;
import swap.lib.AppSwap;
import swap.lib.AppXmrRpc;
import swap.model.request.CancelAndRefundRequest;
import swap.model.request.ResumeRequest;
import swap.model.request.SwapRequest;
import swap.process.ProcessRpcMonero;

import javax.annotation.Nullable;
import java.util.UUID;
import java.util.concurrent.atomic.AtomicBoolean;

public class ClientSwap implements App {
    // Backoff between native retry iterations. A failed native call used to be
    // re-invoked in a tight spin, re-initializing wallets and re-broadcasting on
    // every pass; consecutive failures now wait exponentially longer.
    private static final long RETRY_BACKOFF_INITIAL_MS = 5_000;
    private static final long RETRY_BACKOFF_MAX_MS = 60_000;

    private final AppSwap appSwap;
    private ProcessRpcMonero xmrRpcProcess = null;
    private NetworkParameters xmrRpcParameters;
    private String xmrRpcFilePath;
    private String xmrRpcDaemonAddress;
    private int xmrRpcTorPort;
    private boolean xmrRpcViaForwarder;
    private String xmrRpcTrustSource;
    private java.util.concurrent.ScheduledExecutorService xmrRpcWatchdog;
    private int xmrRpcRestarts;
    @Nullable
    private SwapRequest swapRequest = null;
    @Nullable
    private ResumeRequest resumeRequest = null;
    /** A seller fee rate the user accepted for this one swap (SellerFeeAllowance), or null. */
    @Nullable
    private Long acceptedSellerFeeRate = null;

    public AtomicBoolean running = new AtomicBoolean(false);
    // Set when the swap reaches a terminal outcome (completed/refunded/aborted or
    // a fatal non-restartable error) so the retry loop stops instead of re-invoking
    // the native call on a finished swap.
    private final AtomicBoolean terminal = new AtomicBoolean(false);
    private Thread thread = null;
    private String moneroDaemonUrl;

    public ClientSwap(AppSwap appSwap, SwapRequest swapRequest) {
        this.appSwap = appSwap;
        // Decided before anything starts or any Bitcoin moves: when witnesses are required and too
        // few nodes qualify, the swap is refused here instead of stalling after the lock.
        HelperMoneroWitnesses.Selection witnesses = appSwap.selectMoneroWitnesses();
        if (witnesses.problem() != null) {
            throw new IllegalStateException(witnesses.problem());
        }
        startXmrRpcProcess(swapRequest.params(), swapRequest.proxyPort());
        // Electrum pool forwarder (shared with the ASB): local endpoint, the
        // forwarder Tor-dials the pool. Direct SSL is refused; only an onion
        // tcp:// endpoint may be used when the forwarder is unavailable.
        String electrumUrl = appSwap.getSafeElectrumEndpoint(swapRequest.electrumUrl());
        // Taken once, for this swap only: its own internal retries keep it, the next swap does not.
        this.acceptedSellerFeeRate = swap.model.SellerFeeAllowance.take(swapRequest.libp2pPeerAddress());
        this.swapRequest = new SwapRequest(
                swapRequest.uuid(),
                swapRequest.seedBase64(),
                swapRequest.xmrReceiveAddress(),
                electrumUrl,
                swapRequest.libp2pPeerAddress(),
                swapRequest.proxy(),
                this.xmrRpcProcess.endpoint(),
                swapRequest.params(),
                swapRequest.proxyPort(),
                swapRequest.refundAddress(),
                swapRequest.swapsAccount(),
                swapRequest.swapsReceiveIndex(),
                swapRequest.swapsChangeIndex(),
                swapRequest.dojoUrl(),
                swapRequest.dojoApiKey(),
                this.moneroDaemonUrl,
                appSwap.getTakerElectrumPoolUrls(),
                witnesses.urls(),
                witnesses.required(),
                witnesses.count()
        );
    }

    public ClientSwap(AppSwap appSwap, ResumeRequest resumeRequest) {
        this.appSwap = appSwap;
        // A resume is never refused: the swap may need to redeem or refund. Without enough
        // witnesses the engine just never releases a redeem signature (the flag says so).
        HelperMoneroWitnesses.Selection witnesses = appSwap.selectMoneroWitnesses();
        startXmrRpcProcess(resumeRequest.params(), resumeRequest.proxyPort());
        long swapsAccount = HelperSwapsDb.getInstance().getSwapsAccount(resumeRequest.swapId());
        String electrumUrl = appSwap.getSafeElectrumEndpoint(resumeRequest.electrumUrl());
        this.resumeRequest = new ResumeRequest(
                resumeRequest.seedBase64(),
                resumeRequest.swapId(),
                electrumUrl,
                resumeRequest.proxy(),
                this.xmrRpcProcess.endpoint(),
                resumeRequest.params(),
                resumeRequest.proxyPort(),
                swapsAccount,
                resumeRequest.swapsReceiveIndex(),
                resumeRequest.swapsChangeIndex(),
                resumeRequest.dojoUrl(),
                resumeRequest.dojoApiKey(),
                this.moneroDaemonUrl,
                appSwap.getTakerElectrumPoolUrls(),
                witnesses.urls(),
                witnesses.required(),
                witnesses.count()
        );
    }

    @Override
    public void start() {
        running.set(true);

        thread = new Thread(() -> {
            long backoffMs = 0;
            while (running.get()) {
                if (swapRequest != null) {
                    buyXmr();
                } else if (resumeRequest != null) {
                    resume();
                }

                if (!running.get() || terminal.get()) break;

                // Transient failure: wait before retrying, doubling up to the cap.
                backoffMs = nextBackoffMs(backoffMs);
                sleepInterruptibly(backoffMs);
            }
        });
        thread.start();
    }

    static long nextBackoffMs(long previousMs) {
        if (previousMs <= 0) return RETRY_BACKOFF_INITIAL_MS;
        return Math.min(previousMs * 2, RETRY_BACKOFF_MAX_MS);
    }

    private void sleepInterruptibly(long ms) {
        try {
            long remaining = ms;
            // Sleep in small slices so stop() takes effect promptly.
            while (remaining > 0 && running.get() && !terminal.get()) {
                long slice = Math.min(250, remaining);
                Thread.sleep(slice);
                remaining -= slice;
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }

    /** Marks this swap client terminal so its retry loop stops at the next check. */
    public void markTerminal() {
        terminal.set(true);
        stopXmrRpcWatchdog();
    }

    @Override
    public void stop() {
        running.set(false);
        stopXmrRpcProcess();
        thread = null;
    }

    @Override
    public void restart() {
        stop();
        start();
    }

    public void restartXmrRpcProcess() {
        stopXmrRpcProcess();
        startXmrRpcProcess(getParams(), getProxyPort());
    }

    private void stopXmrRpcProcess() {
        stopXmrRpcWatchdog();
        ProcessRpcMonero processRpcMonero = getXmrRpcProcess();
        if (processRpcMonero == null) return;
        boolean success = processRpcMonero.shutdown();
        if (success) xmrRpcProcess = null;
    }

    private void startXmrRpcProcess(NetworkParameters parameters, int proxyPort) {
        try {
            if (Tor.getDefault() == null) {
                throw new IllegalStateException("Tor is required; refusing to start wallet RPC without Tor");
            }
            if (Tor.getDefault() != null) {
                String forwarderEndpoint = appSwap.startDaemonPoolForwarder();
                String daemonAddress = forwarderEndpoint != null ? forwarderEndpoint : appSwap.getMoneroDaemon();
                this.moneroDaemonUrl = daemonAddress;
                int rpcTorPort = forwarderEndpoint != null ? AppSwap.UNINITIALIZED_TOR_PORT : proxyPort;
                this.xmrRpcProcess = AppXmrRpc.startMoneroRpc(parameters, appSwap.getRpcRootDir().getCanonicalPath(), daemonAddress, rpcTorPort, AppXmrRpc.RPC_SWAPCLIENT, forwarderEndpoint != null, appSwap.getMoneroDaemon());
                this.xmrRpcParameters = parameters;
                this.xmrRpcFilePath = appSwap.getRpcRootDir().getCanonicalPath();
                this.xmrRpcDaemonAddress = daemonAddress;
                this.xmrRpcTorPort = rpcTorPort;
                this.xmrRpcViaForwarder = forwarderEndpoint != null;
                this.xmrRpcTrustSource = appSwap.getMoneroDaemon();
                // The swap engine opens its Monero wallet against this endpoint
                // as soon as it starts; the RPC refuses connections for a few
                // seconds after launch (seen: connection refused, then a swap
                // that only reported the failure after the 87 s Bitcoin sync).
                // Probe instead of letting the engine race the startup.
                if (!AppXmrRpc.waitUntilListening(this.xmrRpcProcess, 30000L)) {
                    System.out.println("[XMR_RPC] swap wallet RPC not accepting connections after 30s; starting the swap anyway");
                }
                startXmrRpcWatchdog();
            }
        } catch (Exception e) {
            throw new RuntimeException(e);
        }
    }

    public ProcessRpcMonero getXmrRpcProcess() {
        return xmrRpcProcess;
    }

    /**
     * Keeps the wallet RPC alive for the duration of the swap. It is the only
     * link between the swap engine and the Monero wallet; if the process dies,
     * every engine retry fails with a connection error until the app restarts.
     * Restart on the same port and credentials so the endpoint already handed
     * to the engine stays valid.
     */
    private void startXmrRpcWatchdog() {
        stopXmrRpcWatchdog();
        xmrRpcRestarts = 0;
        xmrRpcWatchdog = java.util.concurrent.Executors.newSingleThreadScheduledExecutor(runnable -> {
            Thread watchdog = new Thread(runnable, "xmr-rpc-watchdog");
            watchdog.setDaemon(true);
            return watchdog;
        });
        xmrRpcWatchdog.scheduleWithFixedDelay(
                this::checkXmrRpcProcess, 10, 10, java.util.concurrent.TimeUnit.SECONDS);
    }

    private void checkXmrRpcProcess() {
        ProcessRpcMonero current = this.xmrRpcProcess;
        if (current == null || current.process() == null) return;
        if (current.process().isAlive()) return;
        if (xmrRpcRestarts >= 20) {
            System.out.println("[XMR RPC] wallet-rpc keeps exiting; giving up after "
                    + xmrRpcRestarts + " restarts");
            stopXmrRpcWatchdog();
            return;
        }
        xmrRpcRestarts++;
        System.out.println("[XMR RPC] wallet-rpc exited; restarting on the existing endpoint (attempt "
                + xmrRpcRestarts + ")");
        try {
            this.xmrRpcProcess = AppXmrRpc.restartMoneroRpc(
                    current, xmrRpcParameters, xmrRpcFilePath, xmrRpcDaemonAddress,
                    xmrRpcTorPort, xmrRpcViaForwarder, xmrRpcTrustSource);
        } catch (Exception e) {
            System.out.println("[XMR RPC] restart failed: " + e);
        }
    }

    private void stopXmrRpcWatchdog() {
        if (xmrRpcWatchdog != null) {
            xmrRpcWatchdog.shutdownNow();
            xmrRpcWatchdog = null;
        }
    }

    private void buyXmr() {
        if (swapRequest == null) return;
        if (HelperAddress.isLibp2pPeerValid(swapRequest.libp2pPeerAddress()) && HelperAddress.isXmrAddrValid(swapRequest.xmrReceiveAddress())) {
            buyXmrV5(ClientSwapRequests.buy(swapRequest, acceptedSellerFeeRate).toString());
        }
    }

    private void resume() {
        if (resumeRequest == null) return;
        String swapId = resumeRequest.swapId();
        if (isValidUuid(swapId)) {
            resume(resumeRequest.toJson().toString());
        }
    }

    public String cancelAndRefund(CancelAndRefundRequest cancelAndRefundRequest) {
        return cancelAndRefund(cancelAndRefundRequest.toJson().toString());
    }

    private native void buyXmr(String json);

    private native void buyXmrV5(String json);

    private native void resume(String json);

    private native String cancelAndRefund(String json);

    private boolean isValidUuid(String uuid) {
        try {
            UUID.fromString(uuid);
            return true;
        } catch (Exception e) {
            return false;
        }
    }

    private NetworkParameters getParams() {
        return swapRequest != null ? swapRequest.params() : resumeRequest != null ? resumeRequest.params() : null;
    }

    private int getProxyPort() {
        return swapRequest != null ? swapRequest.proxyPort() : resumeRequest != null ? resumeRequest.proxyPort() : -1;
    }
}
