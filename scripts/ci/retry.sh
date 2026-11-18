#!/usr/bin/env bash
# Runs a command up to 3 times (a flaky download is the usual reason a package install fails once and works the next time).
# Usage: retry.sh <command> [arguments...]
# Exits 0 as soon as one attempt works; otherwise exits with the last attempt's exit code, so a real failure is never hidden.
set -u
RETRY_PAUSE="${RETRY_PAUSE:-20}"
for attempt in 1 2 3; do
  "$@"
  code=$?
  [ "$code" -eq 0 ] && exit 0
  if [ "$attempt" -eq 3 ]; then
    echo "::error title=retry::'$*' failed 3 times (last exit code $code)"
    exit "$code"
  fi
  echo "::warning title=retry::attempt $attempt of '$*' failed (exit code $code); trying again in $((attempt * RETRY_PAUSE)) seconds"
  sleep $((attempt * RETRY_PAUSE))
done
