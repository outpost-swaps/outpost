package swap.process;

import org.bitcoinj.core.Coin;
import swap.gui.GUISwap;
import swap.gui.controller.pages.SwapsController;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStreamReader;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

public record ProcessPriceTicker(Process process) {
    private static final Pattern ANSI = Pattern.compile("\u001B\\[[0-9;]*m");
    private static final Pattern ASK = Pattern.compile("ask=([0-9]+(?:\\.[0-9]+)?)");

    public ProcessPriceTicker {
        GUISwap.executorService.submit(() -> {
            BufferedReader reader = new BufferedReader(new InputStreamReader(process.getInputStream()));
            String line;
            try {
                while (((line = reader.readLine()) != null) && process.isAlive()) {
                    if (!line.contains("PRICE_TICKER_UPDATE")) continue;
                    // Never let one malformed line kill the reader for the rest
                    // of the session (the spot would stay frozen at the cache).
                    try {
                        String clean = ANSI.matcher(line).replaceAll("");
                        Matcher matcher = ASK.matcher(clean);
                        if (!matcher.find()) {
                            System.out.println("PRICE_TICKER_UNPARSED " + clean);
                            continue;
                        }
                        Coin priceInBtc = Coin.parseCoin(matcher.group(1));
                        SwapsController swapsController = SwapsController.instance;
                        if (swapsController != null) {
                            swapsController.setXmrPrice(priceInBtc);
                            System.out.println("PRICE_TICKER_APPLIED " + priceInBtc.toPlainString());
                        }
                    } catch (RuntimeException e) {
                        System.out.println("PRICE_TICKER_BAD_LINE " + line + " :: " + e.getMessage());
                    }
                }
            } catch (IOException e) {
                e.printStackTrace();
            }
        });
    }

    public static boolean shutdown(String pid) {
        System.out.println("Shutting down price ticker with PID: " + pid);
        return ProcessGraceful.shutdownPid(pid, "price_ticker");
    }

    public boolean shutdown() {
        if (process != null) {
            System.out.println("Shutting down price ticker: " + process.pid());
            return ProcessGraceful.shutdown(process);
        }
        return false;
    }
}
