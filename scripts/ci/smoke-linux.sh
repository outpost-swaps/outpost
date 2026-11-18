#!/usr/bin/env bash
# Runs a program inside the current display (use it under xvfb-run), waits 25 seconds,
# reports whether it is still running, and takes a screenshot of the display.
# Usage: smoke-linux.sh <screenshot.png> <program> [arguments...]
# Prints one line: "running" or "exited with code N". Always exits 0; the workflow reads the line.
set -u
shot="$1"; shift
mkdir -p "$(dirname "$shot")"
log="${shot%.png}.log"
"$@" > "$log" 2>&1 &
pid=$!
sleep "${SMOKE_SECONDS:-25}"
if kill -0 "$pid" 2>/dev/null; then
  state="running"
else
  code=0
  wait "$pid" || code=$?
  state="exited with code $code"
fi
if command -v import >/dev/null 2>&1; then
  import -window root "$shot" 2>/dev/null || echo "(screenshot failed)" >&2
fi
kill "$pid" 2>/dev/null || true
echo "$state"
exit 0
