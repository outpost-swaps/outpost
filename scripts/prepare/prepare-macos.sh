#!/bin/sh
set -eu

cd "$(dirname "$0")/../.."

# The pinned libz-sys fallback bundles an old zlib that does not compile against
# recent macOS SDK headers. Prefer Homebrew's maintained zlib with pkg-config.
if ! pkg-config --exists zlib; then
    if command -v brew >/dev/null 2>&1; then
        ZLIB_PREFIX="$(brew --prefix zlib 2>/dev/null || true)"
    else
        ZLIB_PREFIX=""
    fi
    if [ -n "$ZLIB_PREFIX" ] && [ -f "$ZLIB_PREFIX/lib/pkgconfig/zlib.pc" ]; then
        ZLIB_PKG_CONFIG="$ZLIB_PREFIX/lib/pkgconfig"
        if [ -n "${PKG_CONFIG_PATH:-}" ]; then
            PKG_CONFIG_PATH="$ZLIB_PKG_CONFIG:$PKG_CONFIG_PATH"
        else
            PKG_CONFIG_PATH="$ZLIB_PKG_CONFIG"
        fi
        export PKG_CONFIG_PATH
    else
        printf '%s\n' "Homebrew zlib is required to build this project's pinned Rust dependencies on current macOS." >&2
        printf '%s\n' "Install it with: brew install zlib" >&2
        printf '%s\n' "Then rerun ./start.sh" >&2
        exit 1
    fi
fi

cd xmr-btc-swap
cargo build --release
# The ASB must be built with the 5.0 protocol feature; the workspace default does not enable it.
cargo build --release -p swap --bin asb --features protocol-v5
mkdir -p ../app/src/main/resources/binaries
cp target/release/asb ../app/src/main/resources/binaries/asb
cp target/release/price_ticker ../app/src/main/resources/binaries/price_ticker
cp target/release/daemon_pool_proxy ../app/src/main/resources/binaries/daemon_pool_proxy
cd ..
cargo build --release
cp target/release/libatomicswap.dylib app/src/main/resources/libatomicswap.dylib

sh scripts/prepare/write-build-manifest.sh
