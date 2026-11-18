# Node-list maintenance

The curated lists in `app/src/main/resources/node-directory.json` go stale. These scripts check them
through Tor so a refresh is one command and not guesswork. All need a Tor SOCKS proxy on
`127.0.0.1:9050` (a system Tor; the app's own Tor uses another port).

- `probe-electrum-servers.py`: connects to each Electrum server in `$TMPDIR/cands.json` (a JSON list of
  `{"url": "tcp://host:port", "src": "...", "net": "mainnet|testnet", "name": "..."}`), asks for the
  version, tip and genesis header, and writes `$TMPDIR/probe_results.json`. Run it twice, hours apart:
  onion servers are slow or flaky now and then, and a node that fails once is not dead.
- `electrum-rtt.py`: round-trip times of a few servers, to see whether a slow sync is one slow server.
- `probe-monero-nodes.py [node-directory.json] [runs]`: `get_info` for every Monero mainnet node in the
  directory file, through Tor, with STRICT TLS (a node that only answers without certificate checks
  cannot be used by the app: the daemon client and the forwarder verify strictly, which is how
  Hashvault and XMRTW had to be removed). Exit status 1 if a node failed every run.

The app also finds Electrum servers by itself at run time (Dojo Bay's directory, probed through Tor
before they are offered), so this is for the packaged base list. See docs/SETTINGS.md.
