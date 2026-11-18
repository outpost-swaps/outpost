#!/bin/sh
# Records the SHA-256 of every artifact the prepare scripts bundle, so the app can
# refuse to run a native library and an asb that were not built together
# (HelperBuildManifest). Run after the artifacts are copied into the resources.
set -eu

cd "$(dirname "$0")/../.."
RES=app/src/main/resources
OUT="$RES/binaries/build-manifest.txt"

hash_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

outer="$(git rev-parse --short HEAD 2>/dev/null || echo nogit)"
nested="$(git -C xmr-btc-swap rev-parse --short HEAD 2>/dev/null || echo nogit)"

{
    echo "# written by scripts/prepare/write-build-manifest.sh; do not edit"
    echo "build_id=$(date -u +%Y%m%dT%H%M%SZ)-$outer-$nested"
    for name in asb price_ticker daemon_pool_proxy asb.exe price_ticker.exe daemon_pool_proxy.exe; do
        [ -f "$RES/binaries/$name" ] && echo "$name=$(hash_of "$RES/binaries/$name")"
    done
    for lib in libatomicswap.dylib libatomicswap.so atomicswap.dll; do
        [ -f "$RES/$lib" ] && echo "$lib=$(hash_of "$RES/$lib")"
    done
} > "$OUT"
echo "Wrote $OUT"
