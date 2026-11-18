#!/bin/sh
# Local CI: every check a change has to pass, in one command. No hosted service needed; a hosted
# CI (GitHub Actions, Forgejo, ...) later only has to call this script.
#
#   scripts/ci/check.sh                 everything below, in order, stopping at the first failure
#   scripts/ci/check.sh --keep-going    run every step and list the failures at the end
#   scripts/ci/check.sh --java-copy DIR run the Java tests in a scratch copy of the project (use
#                                       this while the app is running from this tree: Gradle would
#                                       otherwise rebuild the classes the app is using)
#   scripts/ci/check.sh --docker        also run the tests that start a Monero node in Docker
#   scripts/ci/check.sh --only NAME     run one step: toolchain, data, rust-app, rust-engine, java
#
# Steps:
#   toolchain    JDK / Gradle / JavaFX versions are the supported release line
#   data         node-directory.json is valid JSON (a broken list is a broken app)
#   rust-app     the app's Rust crate (JNI layer, request parsing, ...)
#   rust-engine  the swap engine workspace, with the protocol-v5 feature: unit tests of every crate
#                plus the integration tests that run offline (the Docker-based ones need --docker)
#                and the separate protocol-v5 workspace (swap setup, state machine)
#   java         the GUI/app unit tests (Gradle)
set -eu
cd "$(dirname "$0")/../.."
ROOT=$(pwd)

# The build needs JDK 25 (same lookup as start.sh): JAVA_HOME if it is 25+, else one in ~/.jdks.
java_major() { "$1" -version 2>&1 | sed -n '1s/.*version "\([0-9][0-9]*\).*/\1/p'; }
for home in "${JAVA_HOME:-}" "$HOME"/.jdks/*25*/Contents/Home "$HOME"/.jdks/*25* \
    "$(/usr/libexec/java_home -v 25 2>/dev/null || true)"; do
    if [ -n "$home" ] && [ -x "$home/bin/java" ] && [ "$(java_major "$home/bin/java")" -ge 25 ] 2>/dev/null; then
        JAVA_HOME=$home
        export JAVA_HOME
        PATH="$JAVA_HOME/bin:$PATH"
        export PATH
        break
    fi
done

KEEP_GOING=0
JAVA_COPY=""
DOCKER=0
ONLY=""
while [ $# -gt 0 ]; do
    case "$1" in
        --keep-going) KEEP_GOING=1 ;;
        --java-copy) shift; JAVA_COPY="${1:?--java-copy needs a directory}" ;;
        --docker) DOCKER=1 ;;
        --only) shift; ONLY="${1:?--only needs a step name}" ;;
        -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
        *) printf 'unknown option: %s\n' "$1" >&2; exit 2 ;;
    esac
    shift
done

# Rust debug builds are large (tens of GB per workspace); a full disk shows up as a confusing
# linker error. Stop early with a clear message instead.
free_gb=$(df -g . | awk 'NR==2 {print $4}')
if [ "${free_gb:-0}" -lt 20 ] 2>/dev/null; then
    printf 'Only %s GB of disk is free; the builds need at least 20. Free space first (for example\n' "$free_gb" >&2
    printf 'rm -rf xmr-btc-swap/target/debug target/debug: they are rebuilt on the next run).\n' >&2
    exit 1
fi

FAILED=""
run_step() { # name command...
    name=$1; shift
    if [ -n "$ONLY" ] && [ "$ONLY" != "$name" ]; then
        return 0
    fi
    printf '\n==> %s\n' "$name"
    start=$(date +%s)
    if "$@"; then
        printf '==> %s: ok (%ss)\n' "$name" "$(( $(date +%s) - start ))"
    else
        printf '==> %s: FAILED (%ss)\n' "$name" "$(( $(date +%s) - start ))"
        FAILED="$FAILED $name"
        if [ "$KEEP_GOING" -eq 0 ]; then
            printf '\nFAILED:%s\n' "$FAILED"
            exit 1
        fi
    fi
}

step_toolchain() { ./scripts/release/check-toolchain.sh; }

step_data() {
    python3 -I -c '
import json, sys
d = json.load(open("app/src/main/resources/node-directory.json"))
for net in ("mainnet", "testnet"):
    assert d["electrum"][net], "electrum " + net + " list is empty"
for net in ("mainnet", "stagenet"):
    assert d["monero"][net], "monero " + net + " list is empty"
print("node-directory.json ok")
'
}

step_rust_app() { cargo test --offline; }

step_rust_engine() (
    cd xmr-btc-swap
    if [ "$DOCKER" -eq 1 ]; then
        # Everything, including the integration tests that start Bitcoin and Monero nodes in Docker.
        cargo test --offline --workspace --no-fail-fast --features swap/protocol-v5
    else
        # Unit tests of every crate plus the two integration tests that need no Docker. The other
        # swap/tests/*.rs files start nodes in containers: they wait out a 60 s timeout each and
        # fail without Docker.
        cargo test --offline --workspace --no-fail-fast --lib --bins \
            --exclude monero-wallet --exclude monero-harness --features swap/protocol-v5
        cargo test --offline -p swap --features protocol-v5 --test dial_check --test dojo_fee_estimator
    fi
    # The protocol-5 crates (swap-p2p, swap-machine, ...) are their own workspace, which the one
    # above excludes, so their tests (setup, state machine) only run here.
    cd protocol-v5
    cargo test --offline --workspace --no-fail-fast
)

step_java() {
    if [ -n "$JAVA_COPY" ]; then
        mkdir -p "$JAVA_COPY/app"
        rsync -a --delete "$ROOT/app/src/" "$JAVA_COPY/app/src/"
        (cd "$JAVA_COPY" && ./gradlew :app:test --offline -q)
    else
        ./gradlew :app:test --offline -q
    fi
}

run_step toolchain step_toolchain
run_step data step_data
run_step rust-app step_rust_app
run_step rust-engine step_rust_engine
run_step java step_java

if [ -n "$FAILED" ]; then
    printf '\nFAILED:%s\n' "$FAILED"
    exit 1
fi
printf '\nAll checks passed.\n'
