#!/bin/sh
set -eu

PROJECT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$PROJECT_DIR"

# Dev/test launcher: use the bundled ASB as a seller for self-swap testing.
# The packaged app keeps the release default (reject) — see docs/SECURITY.md.
: "${OUTPOST_ALLOW_SELF_ASB:=1}"
export OUTPOST_ALLOW_SELF_ASB

# JavaFX 25 and the Gradle 9 wrapper need JDK 25 (LTS). Use JAVA_HOME if it is
# 25+, else a JDK 25 from ~/.jdks or /usr/libexec/java_home, else java on PATH.
java_major() {
    "$1" -version 2>&1 | sed -n '1s/.*version "\([0-9][0-9]*\).*/\1/p'
}
pick_jdk() {
    for home in "${JAVA_HOME:-}" "$HOME"/.jdks/*25*/Contents/Home "$HOME"/.jdks/*25* \
        "$(/usr/libexec/java_home -v 25 2>/dev/null || true)"; do
        if [ -n "$home" ] && [ -x "$home/bin/java" ] && [ "$(java_major "$home/bin/java")" -ge 25 ] 2>/dev/null; then
            printf '%s' "$home"
            return 0
        fi
    done
    return 1
}
if JDK_HOME=$(pick_jdk); then
    JAVA_HOME=$JDK_HOME
    export JAVA_HOME
    PATH="$JAVA_HOME/bin:$PATH"
    export PATH
fi
if ! command -v java >/dev/null 2>&1 || [ "$(java_major java)" -lt 25 ] 2>/dev/null; then
    printf '%s\n' "JDK 25 (LTS) is required. Install it (for example Temurin 25) and set JAVA_HOME or put it in ~/.jdks." >&2
    exit 1
fi
printf '%s\n' "Using $(java -version 2>&1 | sed -n '1p') from $JAVA_HOME"

if ! command -v cargo >/dev/null 2>&1; then
    printf '%s\n' "Rust (cargo) is required. Install it from https://rustup.rs" >&2
    exit 1
fi

if [ ! -f "xmr-btc-swap/swap/Cargo.toml" ]; then
    printf '%s\n' "The xmr-btc-swap source is missing. Restore it before building." >&2
    exit 1
fi

case "$(uname -s)" in
    Darwin) PREPARE_SCRIPT="./scripts/prepare/prepare-macos.sh" ;;
    Linux) PREPARE_SCRIPT="./scripts/prepare/prepare-linux.sh" ;;
    *)
        printf '%s\n' "Unsupported OS for start.sh. On Windows, run start.bat." >&2
        exit 1
        ;;
esac

printf '%s\n' "Building native swap binaries and JNI library..."
if [ "${SKIP_PREPARE:-0}" = "1" ]; then
    printf '%s\n' "SKIP_PREPARE=1 set - skipping the native build and using the packaged binaries."
else
    "$PREPARE_SCRIPT"
fi

case "$(uname -s)" in
    Darwin) NATIVE_LIBRARY="app/src/main/resources/libatomicswap.dylib" ;;
    Linux) NATIVE_LIBRARY="app/src/main/resources/libatomicswap.so" ;;
esac

if [ ! -f "$NATIVE_LIBRARY" ] || [ ! -f "app/src/main/resources/binaries/asb" ] || [ ! -f "app/src/main/resources/binaries/price_ticker" ]; then
    printf '%s\n' "Native build completed without producing all app runtime resources." >&2
    exit 1
fi

printf '%s\n' "Starting Outpost..."
exec ./gradlew :app:run
