#!/usr/bin/env python3
"""Probe the curated Monero mainnet nodes through Tor, with STRICT TLS.

Why strict: the app's daemon client (reqwest + rustls) and daemon_pool_proxy (webpki roots)
reject self-signed certificates and host-name mismatches. A node that only answers with
`curl -k` is unusable by the app and, as a witness, would fail every swap. (2026-11-09:
nodes.hashvault.pro was self-signed and opennode.xmr-tw.org had a mismatched certificate;
a first probe that ignored certificates called both healthy.)

Usage: probe-monero-nodes.py [node-directory.json] [runs]
Needs a Tor SOCKS proxy on 127.0.0.1:9050 and curl.
"""
import json
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor

DIRECTORY = sys.argv[1] if len(sys.argv) > 1 else "app/src/main/resources/node-directory.json"
RUNS = int(sys.argv[2]) if len(sys.argv) > 2 else 2
TLS_EXIT_CODES = {35, 51, 58, 59, 60, 77, 82, 83, 90, 91}  # curl: handshake / certificate problems


def nodes(network="mainnet"):
    d = json.load(open(DIRECTORY))["monero"][network]
    return [e["url"] for kind in ("clearnet", "onion") for e in d[kind]]


def probe(url):
    started = time.time()
    # No -k: certificate verification stays on.
    p = subprocess.run(
        ["curl", "-sS", "--max-time", "80", "--socks5-hostname", "127.0.0.1:9050",
         url.rstrip("/") + "/get_info"],
        capture_output=True, text=True, timeout=95)
    seconds = round(time.time() - started)
    if p.returncode != 0:
        kind = "TLS" if p.returncode in TLS_EXIT_CODES else "connect"
        return url, False, seconds, f"{kind}: {p.stderr.strip().splitlines()[0][:60] if p.stderr.strip() else p.returncode}"
    try:
        info = json.loads(p.stdout)
    except ValueError:
        return url, False, seconds, "not JSON"
    if not info.get("mainnet"):
        return url, False, seconds, "not mainnet"
    if not info.get("synchronized"):
        return url, False, seconds, "not synchronized"
    return url, True, seconds, f"h={info.get('height')}"


def main():
    urls = nodes()
    results = {u: [] for u in urls}
    for run in range(RUNS):
        with ThreadPoolExecutor(18) as pool:
            for url, ok, seconds, detail in pool.map(probe, urls):
                results[url].append((ok, seconds, detail))
    bad = 0
    for url, runs in results.items():
        line = "  ".join(("OK " if ok else "FAIL") + f" {s:>2}s {d}" for ok, s, d in runs)
        print(f"{url.replace('http://', '').replace('https://', '')[:44]:46} {line}")
        if not any(ok for ok, _, _ in runs):
            bad += 1
    print(f"\n{bad} of {len(urls)} nodes failed every run")
    sys.exit(1 if bad else 0)


main()
