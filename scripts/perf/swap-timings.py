#!/usr/bin/env python3
"""Print the timeline of every swap log and the ASB start-up syncs, from the app's log folder.

Every slowness report so far was argued from memory; this prints the same numbers for every
swap so a change can be judged on a before/after row. Standard library only.

usage: swap-timings.py [log-dir]
"""
import json
import pathlib
import sys
from collections import defaultdict

log_dir = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else (
    pathlib.Path.home() / "Library/Application Support/outpost-swaps/logs")


def events(path):
    for line in path.open(errors="ignore"):
        if '"timestamp"' not in line:
            continue
        try:
            record = json.loads(line)
        except ValueError:
            continue
        yield record["timestamp"], record.get("fields", {})


def seconds(ts):
    h, m, s = ts[11:19].split(":")
    return int(h) * 3600 + int(m) * 60 + int(s)


print("== swaps (seconds since the swap client started; sync in seconds)")
print(f"{'date':10} {'swap':8} {'sync':>5} {'items':>6} {'quote':>6} {'lock':>6} {'distribution':>13}")
for path in sorted(log_dir.glob("swap-*.log"), key=lambda p: p.stat().st_mtime):
    first = sync = items = quote = lock = distribution = None
    for ts, f in events(path):
        message = f.get("message", "")
        first = first or ts
        if message == "Electrum wallet sync finished" and sync is None:
            sync, items = round(f["elapsed_ms"] / 1000), f.get("addresses")
        elif message == "Bitcoin deposit wallet synced" and sync is None:
            sync = round(int(f["elapsed_ms"]) / 1000)
        elif message == "V5_BOB_QUOTE_RECEIVED" and quote is None:
            quote = seconds(ts) - seconds(first)
        elif message == "V5_BOB_LOCK_PUBLISHED" and lock is None:
            lock = seconds(ts) - seconds(first)
        elif message == "V5_BOB_OUTPUT_DISTRIBUTION_READY" and distribution is None:
            distribution = round(f["elapsed_ms"] / 1000, 1)
    if first and sync is not None:
        name = path.stem[5:13]
        print(f"{first[:10]} {name} {sync:>5} {str(items or '-'):>6} {str(quote or '-'):>6} "
              f"{str(lock or '-'):>6} {str(distribution or '-'):>13}")

print("\n== ASB start-up Bitcoin sync (seconds), by day")
by_day = defaultdict(list)
asb_log = log_dir / "asb.log"
if asb_log.exists():
    for ts, f in events(asb_log):
        if f.get("message") == "ASB_SYNCED_BITCOIN_WALLET":
            by_day[ts[:10]].append(int(f["duration"]))
for day in sorted(by_day):
    values = sorted(by_day[day])
    print(f"{day}  starts {len(values):>2}  median {values[len(values) // 2]:>3}  max {values[-1]:>3}")
