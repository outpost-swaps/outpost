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

# Apple requires the bundle version to start with a non-zero number, and jpackage enforces it.
# For a 0.x release, jpackage gets 1.0.0 as the internal bundle version and the version users
# see (CFBundleShortVersionString, Finder's Get Info) is set to APP_VERSION afterwards.
BUNDLE_VERSION="$APP_VERSION"
if [[ "$APP_VERSION" == 0* ]]; then
  BUNDLE_VERSION="1.0.0"
fi

./scripts/prepare/prepare-macos.sh
./gradlew :app:installDist

rm -rf "$DIST_DIR/$APP_NAME.app"
mkdir -p "$DIST_DIR"

# Use the Outpost icon. If jpackage rejects it, run this whole script again without the icon
# (the cleanup at the top clears any leftovers): a package with the default icon beats no package.
ICON_ARGS=(--icon scripts/package/icons/Outpost.icns)
if [ -n "${NO_ICON:-}" ]; then ICON_ARGS=(); fi
if ! jpackage \
  --type app-image \
  --input app/build/install/app/lib \
  --main-jar app.jar \
  --main-class swap.gui.Main \
  --name "$APP_NAME" \
  ${ICON_ARGS[@]+"${ICON_ARGS[@]}"} \
  --app-version "$BUNDLE_VERSION" \
  --vendor "Outpost" \
  --description "Tor-only BTC/XMR atomic swaps" \
  --dest "$DIST_DIR"; then
  if [ -z "${NO_ICON:-}" ]; then
    echo "WARNING: building with the Outpost icon failed; building again without it" >&2
    exec env NO_ICON=1 "$0"
  fi
  exit 1
fi

/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $APP_VERSION" "$DIST_DIR/$APP_NAME.app/Contents/Info.plist"

# Editing Info.plist after jpackage signed the app breaks its (ad hoc) signature, and macOS then calls the downloaded app
# "damaged". Sign the finished app again, ad hoc, and stop here if the signature does not verify.
codesign --force --deep --sign - "$DIST_DIR/$APP_NAME.app"
codesign --verify --deep --strict "$DIST_DIR/$APP_NAME.app"

echo "Created $DIST_DIR/$APP_NAME.app $APP_VERSION (unsigned; sign/notarize before distribution)"
