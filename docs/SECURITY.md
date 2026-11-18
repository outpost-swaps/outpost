# Security model and limitations

## Contents

- [Status](#status)
- [Transport and routing](#transport-and-routing)
- [Key storage](#key-storage)
- [Swap state and backups](#swap-state-and-backups)
- [Transaction and fee safety](#transaction-and-fee-safety)
- [Trust model](#trust-model)
- [Accepted trade-offs](#accepted-trade-offs)
- [Dependencies](#dependencies)
- [Known limitations](#known-limitations)
- [Reporting](#reporting)

## Documentation

- [Buying XMR](BUY_XMR.md)
- [Selling XMR](SELL_XMR.md)
- [Withdrawing BTC](WITHDRAW.md)
- [Fees](FEES.md)
- [Settings](SETTINGS.md)
- [Swap architecture](ATOMIC_SWAPS.md)
- [Privacy and network routing](PRIVACY.md)
- [Rendezvous points](RENDEZVOUS.md)
- [FAQ](FAQ.md)

## Status

Current release hardening covers Tor-only routing, encrypted secret and swap-state storage, local wallet-RPC authentication, Dojo-required fee selection, and owner-only file permissions. No independent security audit has been completed.

## Transport and routing

- Tor is required and always on. There is no Tor-off mode or direct clearnet option.
- Configured clearnet endpoints are reached through Tor, never directly.
- Onion Electrum and Monero connections use TCP inside Tor and do not add a separate TLS layer.
- Clearnet Electrum TLS is handled by the bundled local forwarder, which uses a current rustls stack. Direct `ssl://` Electrum is rejected; use an onion endpoint or the forwarder.
- The local Electrum forwarder and Monero daemon forwarder dial their pools through Tor.
- `monero-wallet-rpc` is local-only and uses random per-process login credentials. The ASB rejects non-local wallet-RPC URLs.

Tor hides your IP; a Tor exit can still see a clearnet destination. Tor-only routing and onion-only privacy are separate concepts, and onion-only is not a current setting.

## Key storage

- The paired wallet seed is passphrase-encrypted in the app properties file.
- The **public Dojo list** is fetched from Dojo Bay's directory over Tor only. Each entry is a Bitcoin signed message holding the Dojo's pairing; it is listed only if the key recovered from the signature matches the notification address of the payment code inside the message and of the payment code the directory advertises for it, and the URL and API key are taken from the signed text. This proves who published a pairing, not that the Dojo is trustworthy: a Dojo still sees every address in the wallet. A saved copy in the data folder is verified again on every read.
- **Connect wallet** (the first-launch window) takes the seed words, a backup file or a pairing payload in that window only. The words are encrypted with the passphrase straight away into the one record a pairing payload produces, are never written in plaintext, and the window's fields are cleared when it closes. A mistyped passphrase opens a different empty wallet without any error, so the window shows the master fingerprint, a LifeHash picture and the first receive address to compare with the wallet before it is used.
- **Create new wallet** makes 12 BIP39 words (128 bits of entropy) and requires a passphrase, as Ashigaru does. The entropy comes from the operating system's cryptographic generator, read through two independent Java generators (the platform's strongest one and a separately started DRBG that reseeds on every request) and hashed together, so a flaw in either alone cannot weaken the seed. If a generator looks broken (constant or repeating output), no words are made. The words are hidden while three of them are typed back from paper, are stored only encrypted like any other wallet, and can be shown again under **Settings → Wallet → BTC Wallet Seed** after the unlock passphrase. The optional Ashigaru backup file is encrypted with the wallet's passphrase.
- The Dojo API key is passphrase-encrypted at rest. It is decrypted only in Java memory and passed explicitly to the Rust swap client and ASB; Rust never reads the encrypted property or writes the key to disk.
- When the GUI starts the ASB, it passes the root seed through stdin to the `start` command. It does not write a temporary plaintext seed file, and it removes a legacy `asb/<network>/seed.pem` file from older versions. The paired Dojo API key goes the same way, on the line after the seed, not in the ASB's environment, where other processes of the same user could read it.
- The ASB Monero wallet has its own 25-word seed, separate from the paired root seed. It is readable through **Settings → Wallet → XMR ASB Seed → Show Seed** (after the unlock passphrase) while the ASB is running; anyone with those words can spend the ASB's Monero.
- The app properties file and its temporary replacement are owner-readable only on POSIX systems.

## Swap state and backups

- Swap state rows are encrypted with ChaCha20-Poly1305 using a key derived from the wallet seed. Swap IDs, timestamps, and addresses remain plaintext metadata; active swap secrets do not.
- The SQLite database, WAL, and shared-memory files are set to owner-only permissions on Unix.
- A database encrypted with one wallet seed cannot be opened with another seed.
- Daily local backups copy swap databases, `swaps.json`, and ASB databases, but deliberately do not copy seed files. Backups contain wallet activity metadata and are also restricted to the owner where the filesystem supports it.

## Transaction and fee safety

- Bitcoin transaction fees are selected once during swap setup and fixed into the pre-signed transactions. The same transaction is never fee-bumped and CPFP is not used. v5 intentionally lets redeem replace an unconfirmed cancel via RBF; that is a different transaction, not a fee bump of the redeem.
- The shipped client and ASB require the paired Dojo for fee estimation. Missing or failed Dojo credentials fail setup closed; they do not fall back to Electrum. Withdraw also never uses Electrum.
- Bitcoin broadcasts try Dojo first and may fall back to Electrum only after Dojo attempts fail or time out. The retried transaction is byte-identical, so the fallback cannot create a conflicting transaction.
- Release builds reject your own ASB as a seller. `OUTPOST_ALLOW_SELF_ASB=1` is for test setups only.

## Trust model

The app trusts the configured Dojo, Electrum, and Monero servers for the data they serve. Tor hides your network location; it does not make a malicious server honest. A server can withhold or delay data, serve a stale chain view, or observe wallet queries made to it.

**Confirmations are proven, not taken on a server's word.** Before a swap treats a Bitcoin transaction as confirmed (the maker's check that the buyer's lock is final before it sends Monero, and the buyer's early-refund, cancel, refund and punish checks), it asks the Electrum servers for a merkle proof that the transaction is in a block and for that block's header, and checks both. The header must meet its own proof-of-work target and, on mainnet, a minimum difficulty, so a server cannot invent a block. One valid proof is enough, so a lying server cannot outvote an honest one; if no server can prove a claimed confirmation, the transaction is treated as unconfirmed and the swap keeps waiting. The paired Dojo's confirmation count is used only when Electrum lags, and only after the same proof succeeds for the block the Dojo names. This is why turning on Electrum failover, which adds the curated pool and servers discovered through Dojo Bay, does not let those servers fake a confirmation. They can still hide or delay transactions, report a wrong chain tip (which shifts when timelocks look expired; the network enforces the real timelocks), and see the addresses they are asked about. On testnet there is no minimum difficulty, because test networks mine minimum-difficulty blocks, so a server willing to mine a trivial header could still forge a proof there.

**Electrum servers found through Dojo Bay** are offered only after a probe through Tor (version, chain tip, and the genesis block's hash for the right network; tips are compared with the median so a server claiming a far higher tip cannot make honest ones look behind). The address of a Dojo's Electrum server is not part of the signed message, so the directory's word is not enough on its own; in the connect window they are offered and tested, but one is preselected only while none of the packaged servers passes, and only onion servers qualify. Any Electrum server still sees every address it is asked about.

The paired Dojo is part of the wallet and fee path. Electrum is part of the ASB Bitcoin path. A Monero daemon can observe the wallet's sync pattern unless it is trusted or operated locally.

## Accepted trade-offs

These are known, bounded limits of the protocol layer, recorded so they are decisions and not accidents (audit reviews of 2026-11-06):

- **Prevout checks trust any single Electrum server.** The maker accepts a lock input reported by any one server in its pool, including an unconfirmed parent. This cannot put funds at risk: the maker locks its Monero only after Bob's lock transaction is final, and a transaction with fake inputs cannot confirm. The cost is griefing: a compromised pool server, or a parent that is later replaced, can hold maker liquidity until the verification deadline. Per-peer and global queue limits and a cooldown bound it.
- **Monero witness nodes make a single dishonest node insufficient.** On mainnet the lock check needs two further independent onion or https nodes to agree with the main one (see Settings). It is skipped only for a local node, or one declared with Node Type set to Personal node (which is always pinned, never failing over to public nodes). An attacker would now have to control at least two distinct operators, or the user's own node. It does not help if every node asked is compromised. The idea comes from Ashigaru Desktop's `monero-witness-nodes` patch (GPL-3.0).
- **A hostile Monero daemon can raise a sweep's fee, up to a cap.** Sweeps take the fee rate from the daemon; it is capped at 2,000,000 piconero per weight unit (about 0.004 XMR on a typical sweep, 20 times the normal mainnet rate). It cannot forge outputs, because ownership is checked from your keys, but it can bias decoy selection and delay or lie about transaction status.
- **If the local daemon forwarder cannot start, the wallet RPC connects to your saved daemon through Tor directly.** For an `https://` daemon it accepts any certificate in that fallback (most Monero daemons are self-signed); the daemon client in the swap engine never connects to a remote daemon without Tor and verifies certificates normally, and falls back to the wallet RPC if that fails.
- **A lock transaction that can never confirm is a griefing cost, not a loss.** Time-based lock times and relative-delay inputs are rejected at intake; a block-height lock time far in the future is not (it needs the chain tip). The maker's wait for the lock to appear is bounded and ends the swap safely.
- **Monero reorgs deeper than the finality depth.** The redeem signature is released after the Monero lock reaches 10 confirmations and is re-checked for a double spend; a reorg deeper than that is not watched for.
- **Ownership proofs support P2WPKH inputs only.** The swap wallet is built from P2WPKH descriptors, so it cannot select other script types.
- **When an external redeem address is configured, the maker's redeem and punish payouts both go to it.** That address is where the maker receives Bitcoin.
- **One-block Bitcoin finality** is the chosen setting.

## Dependencies

Dependencies are triaged against the runtime graph before release. At the last release review there were no reachable critical advisories after patching and filtering.

The older bdk/electrum-client TLS stack remains in the dependency graph, but shipped GUI swap and ASB paths reject direct `ssl://` Electrum and use the local forwarder. Upgrading that stack is post-release work.

## Known limitations

- No independent security audit has been completed.
- The app is a hot-wallet design; compromise of the machine or app data directory can expose wallet material.
- Recovery depends on the saved swap state and waiting for on-chain timelocks.
- A new ASB's first Monero wallet sync can take several minutes over Tor (buying needs no Monero wallet sync: the XMR is swept straight from the daemon).
- Mixing is opt-in, isolated from swap execution, and not heavily tested.
- Logs and backups contain wallet activity metadata. Review them before sharing and never share seeds, private keys, or Dojo API keys.

## Reporting

Report suspected vulnerabilities privately to the project maintainer before public disclosure. Do not include seeds, private keys, Dojo API keys, or full logs containing wallet data.
