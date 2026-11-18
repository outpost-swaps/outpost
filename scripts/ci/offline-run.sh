#!/usr/bin/env bash
# Runs a command with no network at all except the loopback interface, so a test of the app can never reach the internet.
# Usage: offline-run.sh <command> [arguments...]
# If it cannot guarantee that, it REFUSES to run the command (exit 97). It never falls back to running with a network.
set -u
[ "$#" -gt 0 ] || { echo "usage: offline-run.sh <command> [arguments...]" >&2; exit 2; }

# "Is there any way out?" is answered from /proc/net, which follows the current network namespace
# (/sys/class/net does not, and tunnel pseudo-devices such as tunl0 exist even with no network).
no_way_out() {
  [ -r /proc/net/route ] || return 1
  [ "$(grep -c . /proc/net/route)" -le 1 ] || return 1                 # IPv4: only the header line, no routes
  ! grep -v -E ' lo$' /proc/net/if_inet6 2>/dev/null | grep -q .       # IPv6: no address except loopback
}
# Already isolated (for example a container started with --network none): nothing to do.
if no_way_out; then exec "$@"; fi

command -v unshare >/dev/null 2>&1 || { echo "offline-run: 'unshare' is missing, so the command was NOT run" >&2; exit 97; }
SUDO=""; [ "$(id -u)" != 0 ] && SUDO="sudo -E"
# A new network namespace has no interfaces; bring loopback up (the app talks to its own Tor and helpers on 127.0.0.1),
# then check again from the inside and refuse if a way out is still there.
exec $SUDO unshare -n sh -c '
  ip link set lo up 2>/dev/null
  if [ "$(grep -c . /proc/net/route)" -gt 1 ]; then echo "offline-run: still a route out, the command was NOT run" >&2; exit 97; fi
  exec "$@"' sh "$@"
