package swap.listener.impl;

import swap.gui.GUISwap;
import swap.gui.controller.MainController;
import swap.gui.controller.pages.LiquidityController;
import swap.gui.controller.pages.SwapsController;
import swap.listener.AsbListener;
import swap.model.*;

public class AsbListenerImpl implements AsbListener {
    private final MainController mainController;
    private final SwapsController swapsController;
    private final LiquidityController liquidityController;

    public AsbListenerImpl(
            MainController mainController,
            SwapsController swapsController,
            LiquidityController liquidityController) {
        this.mainController = mainController;
        this.swapsController = swapsController;
        this.liquidityController = liquidityController;
    }

    public void printSwapLogLn(String message) {
        mainController.printSwapLogLn(LogType.INFO, message, false);
    }

    public void printSwapLogLnAndKill(String message, boolean clearfields) {
        mainController.printSwapLogLn(LogType.INFO, message, true);
        swapsController.swapKillReset(clearfields);
    }

    @Override
    public void onAsbInitialized(AsbInitData data) {
        GUISwap.appAsb.setPeerId(data.peerId());
        GUISwap.appAsb.setExternalAddress(data.multiaddr());
        printSwapLogLn("[ASB] Initialized with libp2p peer ID: " + data.peerId());

        double unlockedXmrBalance = (double) data.unlockedXmrBalance() / 1000000000000D;
        double lockedXmrBalance = (double) data.lockedXmrBalance() / 1000000000000D;

        mainController.setAsbInitialized();

        liquidityController.setupAsb(data, unlockedXmrBalance, lockedXmrBalance);

        // Mixing is tied to the ASB's own lifecycle, not general wallet setup - it only
        // ever makes sense to run while the ASB is actually up (see MixingService's
        // decoupling invariant: this is best-effort and must never affect the ASB itself).
        if (swap.helper.HelperProperties.isAutoTx0()) {
            String dojoUrl = swap.helper.HelperProperties.dojoUrl;
            swap.wallet.SwapsWallet swapsWallet = swap.wallet.SwapsWallet.getInstance();
            if (dojoUrl != null && !dojoUrl.isEmpty() && swapsWallet != null) {
                swap.mixing.MixingService.enable(swapsWallet.getHdWallet(), GUISwap.appSwap.getParams(),
                        dojoUrl, swap.helper.HelperProperties.dojoApiKey);
            }
        }
    }

    @Override
    public void onAsbXmrBalanceData(AsbXmrBalanceData data) {
        if (!data.error().isEmpty())
            System.err.println("ERROR XMR BALANCE: " + data.error());

        long total = data.total();
        long unlocked = data.unlocked();
        long locked = total - unlocked;
        double totalXmrBalance = (double) total / 1000000000000D;
        double unlockedXmrBalance = (double) unlocked / 1000000000000D;
        double lockedXmrBalance = (double) locked / 1000000000000D;

        mainController.updateGui(() -> {
            liquidityController.xmrBalanceLarge.setText(totalXmrBalance + " XMR");
            liquidityController.lockedXmr.setText(String.valueOf(lockedXmrBalance));
            liquidityController.unlockedXmr.setText(LiquidityController.fmtXmr(unlockedXmrBalance));
        });
    }

    @Override
    public void onAsbBtcBalanceData(String accountName, AsbBtcBalanceData data) {
        liquidityController.onBtcBalanceData(accountName, data);
    }

    @Override
    public void onAsbError(SwapError swapError) {
        if (swapError.fatal()) {
            GUISwap.appAsb.setRunningAsb(false);
            printSwapLogLnAndKill("[ASB] ERROR:: " + swapError.errorType() + ", message: " + swapError.errorMessage(), false);
        } else {
            printSwapLogLn("[ASB] ERROR:: " + swapError.errorType() + ", message: " + swapError.errorMessage());
        }
    }
}
