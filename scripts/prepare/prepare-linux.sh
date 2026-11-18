#!/bin/sh
set -eu

cd "$(dirname "$0")/../.."
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
cp target/release/libatomicswap.so app/src/main/resources/libatomicswap.so

sh scripts/prepare/write-build-manifest.sh
