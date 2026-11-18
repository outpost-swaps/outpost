# Outpost FAQs

Learn more about building and running the program [here](../README.md).

Learn more about using the GUI [here](BUY_XMR.md).

Learn more about selling XMR by running the Automated Swap Backend [here](SELL_XMR.md).

Learn more about configuring the settings [here](SETTINGS.md).

Learn more about swaps architecture [here](ATOMIC_SWAPS.md).

## What BTC and XMR wallet do I need to use?
Outpost is built around [Ashigaru](https://ashigaru.rs): bring in an Ashigaru wallet from its backup file, its seed words or (older versions) its pairing payload, on Mainnet or Testnet. You do not need one to start, though: Outpost can create a wallet for you, made the way Ashigaru makes one, so it opens in Ashigaru Mobile later (see [I created my wallet in Outpost](#i-created-my-wallet-in-outpost-how-do-i-open-it-elsewhere)). Standard BIP39 wallets such as Sparrow work too (see [below](#can-i-use-a-wallet-other-than-ashigaru-such-as-sparrow)). Ashigaru requires a Dojo backend to operate; see [Do I need a Dojo?](#do-i-need-a-dojo) below. It is up to you which XMR wallet you use. ([Monfluo](https://codeberg.org/Monfluo/monfluo-android) is recommended).

## Can I use a wallet other than Ashigaru, such as Sparrow?

Yes. Outpost is built for Ashigaru, but it derives every account itself from the seed, using the standard BIP39/BIP32 derivation that Sparrow and other wallets use (checked against the published BIP84 test vector; no live swap has been run with a Sparrow-created wallet yet). In **Connect wallet**, choose **Seed words** and enter any standard single-signature BIP39 seed (12 or 24 words, plus its BIP39 passphrase if it has one). Outpost derives its own swap accounts from the seed (see [Bitcoin accounts used by swaps](../README.md#bitcoin-accounts-used-by-swaps)), so the other wallet's account layout does not matter for swapping. Things to know:

- **Check the wallet before you continue.** The window shows the master fingerprint and the first address at `m/84'/0'/0'/0/0`. For a Native SegWit (BIP84) wallet such as Sparrow's default, that is the wallet's own first receive address; for other script types, compare the fingerprint.
- **Passphrase:** enter the wallet's BIP39 passphrase only if it has one; a wrong or missing one opens a different, empty wallet without an error, which the fingerprint check catches. A wallet without a passphrase works too: Outpost then asks you to choose a password in step 3 to encrypt it on this computer.
- **You still need a Dojo.** Seed words carry no Dojo, so pick a public one or your own in step 2.
- **Swap funds live in Outpost's own accounts,** at account numbers other wallets do not scan by default. Use Outpost's **Withdraw** page to move them out, or, in Sparrow, add a wallet from the same seed with the custom derivation path (for example `m/84'/0'/2147483643'` for `SWAPS_DEPOSIT` on mainnet).
- **Your other wallet's main account shows up as `DEPOSIT`** on the Withdraw page: listed for visibility, not spendable from Outpost.
- **Not supported:** Electrum-format seeds, 18-word seeds, multisig, and hardware wallets whose seed never leaves the device.
- Entering seed words puts that wallet's keys on this computer, encrypted, in a hot wallet. Consider a separate wallet for swapping.

## I created my wallet in Outpost: how do I open it elsewhere?

Outpost makes a new wallet the way Ashigaru does: 12 BIP39 words plus a required BIP39 passphrase.

- **[Ashigaru Mobile](https://ashigaru.rs):** restore from the backup file Outpost saved (step 3, **Save backup file...**) and enter the passphrase. That brings the Dojo too. Or restore with the 12 words and the passphrase. Outpost's swap accounts show up in Ashigaru.
- **[Sparrow](https://sparrowwallet.com):** the words and passphrase open the standard account only; the swap accounts need custom derivation paths (see [above](#can-i-use-a-wallet-other-than-ashigaru-such-as-sparrow)).

Compare the fingerprint and first address with what Outpost showed. A mistyped passphrase opens a different, empty wallet without any error.

## Do I need a Dojo?

Yes, whichever wallet you use, including one created in Outpost. A public Dojo from the built-in list is enough; you do not have to run your own. Ashigaru requires a Dojo backend to operate at all. Outpost uses the paired Dojo for wallet API data, balances, UTXOs, and Bitcoin fee estimates. Paired swap and ASB fee selection fails closed if Dojo is unavailable; Withdraw offers a manual sat/vB entry instead. Dojo is also tried first for Bitcoin broadcasts, with two 15-second attempts; Electrum is only the broadcast transport fallback. You choose the Dojo when you connect (a pairing payload or backup file brings its own). Use a public Dojo such as [Dojo Bay](https://dojobay.org), or run your own with [Dojo OSP](https://dojo-osp.org) ([source code](https://github.com/Dojobay/dojobay)) or [RoninDojo](https://ronindojo.io). Dojo access is Tor-only and has no clearnet fallback.

## Can I test with Testnet/Stagenet?
Yes. Connect a Testnet Ashigaru wallet and the app configures itself for Testnet/Stagenet. Outpost's Bitcoin test network is **testnet4** (get coins from a testnet4 faucet), paired with Monero stagenet. Eigenwallet's public testnet sellers use testnet3, so they cannot complete swaps with Outpost on testnet. You can even use your ASB to receive the stagenet XMR if you don't want to create a stagenet XMR wallet. ANON/NERO, Feather Wallet, & Monerujo are options to use stagenet as well.

## Why did a swap stop with "This seller's fees are higher than the network needs"?

Every seller states the fee rate its swap transactions will be priced at. Outpost refuses a rate more than three times what the network currently needs, which protects you from a seller inflating fees, and stops before any Bitcoin is locked. Some sellers' fee estimators simply read high on a quiet network. You can **try another seller**, or **accept** that seller's rate for one retry. Accepting costs nothing extra on a successful swap (the seller pays its own redeem fee); only a failed, refunded swap pays the higher rate, and the dialog shows roughly how much. See [Buying XMR](BUY_XMR.md#starting-a-swap).

## What is the fee to swap?
The ASB's market spread is established by the free market; providers of XMR liquidity can set their own percentage fee over or under the XMR spot price (MoneroPriceNow's public XMR/BTC price API, with Feather Wallet's price websocket and then Kraken's REST ticker as automatic fallbacks). This is set on the [Settings](SETTINGS.md) page. Bitcoin miner fees are separate; see [Bitcoin Fee Selection](FEES.md).

## How do I buy XMR?
Read [here](BUY_XMR.md).

## How do I sell XMR?
You must run the Automated Swap Backend. Read [here](SELL_XMR.md).

## Why are balances unavailable?
Ashigaru wallet balances and UTXOs are queried from the paired Dojo over Tor. If Dojo is slow or unavailable, the UI reports the affected data as unavailable and retries; it does not fall back to clearnet. Whirlpool mixing uses the same Dojo-backed wallet data, so it may be unavailable or fail to refresh while Dojo is down. This is separate from Electrum and rendezvous connectivity used by swap discovery and execution.

## What is ASB?
Automated Swap Backend. It is how you become a market maker / liquidity provider in order to sell XMR.

## How does the ASB Min & Max work?
Min = Minimum quantity for a trade with your ASB. If your ASB's XMR balance's BTC equivalent falls below this amount, the ASB will not quote above its available balance. Set the minimum in the Settings page; it must be a valid positive BTC amount.

Max = Maximum quantity for a trade with your ASB. If your ASB's XMR balance's equivalent BTC value is below this amount, then the max amount shown in the Rendezvous lists will be that lower value.

Example: If my max BTC limit is 1 BTC in my [Settings](SETTINGS.md) tab, but I only have ~132 XMR in my ASB wallet, then the "Max BTC" in the seller's list will be ~0.5 BTC (at current spot price).

## How do I know if my ASB is showing up on the Sellers list?
The ASB icon indicates that the backend initialized; its tooltip distinguishes a running ASB from one currently listed with rendezvous peers. The Liquidity page shows the same status.

## How are ASBs found?
Each client connects to its configured rendezvous peers. Those peers provide a list of ASBs that have announced themselves. Additional peers can be configured on the [Settings](SETTINGS.md) page. A local Tor-ready indicator does not guarantee that a particular rendezvous, Dojo, Electrum, or Monero node is reachable; the Dojo indicator reports its own connection result.

If you and someone else wants to trade but don't have a Rendezvous server, they can send you their ASB libp2p address (like the ones you see in the sellers list now) directly via XMPP, SimpleX, Soroban, or something similar, and you can enter it in the seller text field in the Swaps program, and start a trade like normal.

## Can I trade with ASBs which are not using the Outpost application?
Only ASBs speaking the same swap-setup version. Outpost buys and sells with swap-setup `5.0.0`, the protocol of current Eigenwallet (5.0.2), so it is compatible with Eigenwallet sellers and buyers (not yet heavily tested), and is built to work with Ashigaru Desktop's upcoming swap feature as a buyer; see [Client compatibility](ATOMIC_SWAPS.md#client-compatibility). Peers still on COMIT swap-setup `1.0.0` may appear in the seller list but fail at setup; legacy entries are recovery-only.

## What could cause a trade from being interrupted?
This would most likely be caused by a drop in your internet, Tor, or XMR node connection, or if the ASB stops listening during a swap or has disconnected.

## What happens if a swap gets interrupted?
Swap recovery depends on the last persisted protocol state and Bitcoin/Monero timelocks. Keep the GUI running during active swaps. After a restart, unlock the app and it resumes unfinished swaps automatically; check each swap's state in History rather than assuming a timeout means no on-chain transaction was sent.

The current v5 swap path configures a **30-block** Bitcoin cancel timelock and a **144-block** punish timelock on mainnet; testnet/stagenet uses **42** and **72** blocks, respectively. Legacy 1.0 recovery-only swaps used 72/72 on mainnet and 12/6 on testnet; those values do not apply to new v5 swaps. These are protocol-relative lock intervals, not confirmation targets or guarantees that a recovery action has completed. Confirm transactions on-chain and do not close the app until the swap reaches a terminal state.

See [swap recovery details](BUY_XMR.md) for current recovery limitations.

## What is the flow of transactions?
**Successful Path:** <br>
Transaction to SWAPS_DEPOSIT (native SegWit) -> LOCK transaction (P2WSH) -> REDEEM transaction to SWAPS_ASB (native SegWit) -> Bob redeems XMR

**Unsuccessful, refund path:** <br>
SWAPS_DEPOSIT -> LOCK -> (cancel timelock expires) -> CANCEL transaction (to another P2WSH UTXO) -> REFUND transaction (to SWAPS_REFUNDS, native SegWit) -> Alice refunds her XMR

**Unsuccessful, punish path:** <br>
SWAPS_DEPOSIT -> LOCK -> (cancel timelock expires) -> CANCEL -> (punish timelock expires) -> PUNISH transaction (to SWAPS_ASB, native SegWit) -> XMR remains locked by the protocol

## Does this run over Tor?
Yes. The GUI requires Tor and has no Tor-off mode, and it makes no direct clearnet connections. Dojo, Electrum, Monero, rendezvous, and swap traffic go through Tor. Curated Electrum/Monero fallbacks are used only if you enable the failover settings (off by default); some of them are clearnet addresses reached through Tor, so they use a Tor exit; onion-only operation is under consideration. See [Privacy and network routing](PRIVACY.md). The status icon reports local Tor readiness; it does not certify reachability of every remote service.

## What is the source of the BTC?
For a buyer, BTC is selected from the paired Ashigaru accounts shown in the Withdraw/Swaps UI. As an ASB, received BTC goes to its separate ASB account. Auto-Tx0 can optionally mix eligible ASB BTC from the [Settings](SETTINGS.md) page.

## How is the XMR wallet generated?
The app uses [monero-wallet-rpc](https://www.getmonero.org/resources/developer-guides/wallet-rpc.html) for its XMR wallet. The ASB's Monero wallet has its own 25-word seed: write it down from **Settings → Wallet → XMR ASB Seed → Show Seed** (it asks for your unlock passphrase). The wallet files are in the application data folder (**Settings → Wallet → Data Folder → Open**); exact paths vary by operating system and network. Keep wallet keys private and do not share them in diagnostics.

## My Ashigaru has no "Pair to Swaps GUI" option. How do I connect?

Press **Connect wallet** on the first-launch screen and choose **Backup file** (Ashigaru's encrypted backup, with the wallet's passphrase) or **Seed words**. Compare the fingerprint and first receive address the window shows with your wallet before you continue: a mistyped passphrase opens a different, empty wallet without any error. Details are in [Buying XMR](BUY_XMR.md#pairing).

## How do I change the Dojo or use a different wallet?
To change only the **Dojo**, use Settings → Wallet → Dojo → **Change...** (see [Settings](SETTINGS.md#dojo)); no reset is needed. A different **wallet** still needs a reset: there is no UI for that yet.
If you wish to completely reset the app, you can back up the following folder then delete it. It is recommended to back it up in case you have any incomplete swaps you might want to return to soon, as swaps cannot simply be restored from the seed words or a backup file.

The folder can be opened directly with **Data Folder → Open** on the [Settings](SETTINGS.md#wallet-and-recovery) page.

The folder:<br>
**Linux:** `~/.local/share/outpost-swaps`<br>
**Windows:** `%APPDATA%/outpost-swaps`<br>
**macOS:** `~/Library/Application Support/outpost-swaps`

If you used a test build from before the app was named Outpost, the app moves its old folder to this name the first time it starts, so your wallets and swap history carry over. Nothing is copied or deleted.

## Why does a swap ask other Monero nodes, and what does "Personal node" do?

Before releasing the Bitcoin redeem signature, your client checks that the seller's Monero lock really exists. One dishonest Monero node could lie about that, so on mainnet two further independent nodes must agree. If you run your own node, set **Node Type** to **Personal node** in Settings: the node is pinned, never replaced by a public one, and trusted alone. Public nodes keep the extra check. This is separate from Trust Daemon, which only makes wallet sync faster and is always on for your own node. Details are in [Settings](SETTINGS.md).

## What can be self-hosted?
Everything can be hosted yourself, except for the price sources: the primary is MoneroPriceNow's public HTTP price API (`api.moneropricenow.com/xmr-btc.json`, no auth, ~30s updates), the websocket fallback is Feather's ticker (onion address, Kraken protocol), and the tertiary fallback is Kraken's public REST ticker. All are routed through Tor. Self-hosting a price source of your own remains a future goal.

The Dojo, Electrum, and Monero nodes of course can be self-hosted, and so can **rendezvous points** — the "phone book" servers that wallets use to find sellers. Every extra operator strengthens discovery for the whole network: maker listing only works while at least one rendezvous point is reachable. Rendezvous points see no swap details and hold no funds. See [Running a Rendezvous Point](RENDEZVOUS.md).

## What Java version do I need?
None for a release download: it bundles its own Java runtime. Building from source needs JDK 25 (see the [README](../README.md#build-tools)).

## What does "CBOR Error" mean when starting a swap?
It usually means a malformed or incompatible seller response. Update to the latest release, retry, and try another seller; if it persists, include the app log in a bug report. Do not reuse a seller that repeatedly fails this way.

## My swap failed before any Bitcoin was locked. What now?
Sellers are independent and some fail at times: a price feed that is down, an offer that does not match the seller's own quote, an amount outside its limits. Outpost stops the swap before the Bitcoin lock, so nothing is spent and your deposit stays in your swap wallet. Pick another seller and try again; see [When a seller fails during setup](BUY_XMR.md#when-a-seller-fails-during-setup).

## How do I troubleshoot an "Error initializing Monero wallet" message?
The line says why. Most often it is "the Monero node did not answer over Tor": the first connection to a node, especially an onion one on a fresh install, can be slow. The ASB retries the sync for about two and a half minutes, then the app restarts it with growing pauses, so it usually recovers by itself. If it keeps failing, pick a different Monero node in Settings. If the message mentions a locked wallet file, another `monero-wallet-rpc` or `asb` may still be running: check with `ps aux | grep monero-wallet-rpc` and stop the extra one.
