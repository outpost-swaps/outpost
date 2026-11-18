package swap.model;

public class Changelog {
    public static final String TEXT =
        "- Rebuilt Bitcoin and Monero atomic swaps on the current Eigenwallet swap protocol (5.0)\n" +
        "- Clear swap progress, with transaction ids and regular updates while the Bitcoin lock confirms and the Monero is swept\n" +
        "- Safer recovery: an interrupted swap resumes from its last saved state, and a stored Monero sweep is resent instead of rebuilt\n" +
        "- One guided Connect wallet window: pair with Ashigaru's payload, or import its backup file or your seed words, and check the fingerprint before you continue\n" +
        "- No wallet yet? Create one here: 12 words and a passphrase, made the way Ashigaru makes them, with an Ashigaru backup file to restore it on your phone\n" +
        "- Balances and the Withdraw page rebuilt on your Dojo, so a Whirlpool outage no longer locks up the app\n" +
        "- Coinjoin is back, rebuilt on Ashigaru's Whirlpool\n" +
        "- Private by default: your Dojo, Electrum servers, Monero nodes and swap peers are all reached over Tor\n" +
        "- Choose a public Dojo from Dojo Bay's signed list, or use your own, and test the connection from Settings\n" +
        "- Public or Personal Monero nodes and Electrum servers, with failover only if you turn it on\n" +
        "- An updated Liquidity page: balances, your rate against the market, earnings and activity, with the ASB started from the same page and optional auto-mixing of the BTC it earns\n" +
        "\n" +
        "Confused about the status of a swap? Just leave the swap client or ASB running, if over half a day has passed and you're still confused, contact the developers.";
}
