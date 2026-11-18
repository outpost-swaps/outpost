#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/../.."

APP_NAME="Outpost"
APP_VERSION="${APP_VERSION:-0.1.0}"
DIST_DIR="${DIST_DIR:-dist}"

if ! [[ "$APP_VERSION" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]]; then
  echo "APP_VERSION must be up to three numbers, e.g. 0.1.0: $APP_VERSION" >&2
  exit 1
fi

./scripts/prepare/prepare-linux.sh
./gradlew :app:installDist

rm -rf "$DIST_DIR/$APP_NAME"
mkdir -p "$DIST_DIR"

# Use the Outpost icon. If jpackage rejects it, run this whole script again without the icon
# (the cleanup at the top clears any leftovers): a package with the default icon beats no package.
ICON_ARGS=(--icon scripts/package/icons/outpost.png)
if [ -n "${NO_ICON:-}" ]; then ICON_ARGS=(); fi
if ! jpackage \
  --type app-image \
  --input app/build/install/app/lib \
  --main-jar app.jar \
  --main-class swap.gui.Main \
  --name "$APP_NAME" \
  ${ICON_ARGS[@]+"${ICON_ARGS[@]}"} \
  --app-version "$APP_VERSION" \
  --vendor "Outpost" \
  --description "Tor-only BTC/XMR atomic swaps" \
  --dest "$DIST_DIR"; then
  if [ -z "${NO_ICON:-}" ]; then
    echo "WARNING: building with the Outpost icon failed; building again without it" >&2
    exec env NO_ICON=1 "$0"
  fi
  exit 1
fi

# Extras inside the folder, so the tar.gz gives people a menu entry with the Outpost icon:
# a plain archive cannot carry an icon, so ship the icon and a small script that registers it.
cp scripts/package/icons/outpost.png "$DIST_DIR/$APP_NAME/Outpost.png"
cp scripts/package/linux/install-menu-entry.sh "$DIST_DIR/$APP_NAME/install-menu-entry.sh"
cp scripts/package/linux/README.txt "$DIST_DIR/$APP_NAME/README.txt"
chmod +x "$DIST_DIR/$APP_NAME/install-menu-entry.sh"

echo "Created $DIST_DIR/$APP_NAME (unsigned; sign/checksum before distribution)"
