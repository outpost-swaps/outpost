#!/usr/bin/env bash
# Builds an AppImage from the packaged app folder (dist/Outpost). Usage: build-appimage.sh <output file>
# An AppImage is a small runtime program followed by a compressed (squashfs) copy of the app folder.
# Both inputs are pinned: the runtime by version and SHA-256, the squashfs by the standard mksquashfs tool.
# Needs: squashfs-tools (mksquashfs) and, unless RUNTIME_FILE is set, curl.
set -euo pipefail
cd "$(dirname "$0")/../../.."

OUT="${1:?give the output file name, for example outpost-0.1.0-linux-x64.AppImage}"
APP_DIR="${APP_DIR:-dist/Outpost}"
RUNTIME_URL="https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64"
RUNTIME_SHA256="2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"

[ -x "$APP_DIR/bin/Outpost" ] || { echo "no packaged app at $APP_DIR (bin/Outpost is missing)" >&2; exit 1; }

work="$(mktemp -d)"
appdir="$work/AppDir"
mkdir -p "$appdir"
cp -a "$APP_DIR" "$appdir/Outpost"
cp scripts/package/icons/outpost.png "$appdir/outpost.png"
ln -s outpost.png "$appdir/.DirIcon"
cp scripts/package/linux/appimage/AppRun "$appdir/AppRun"
cp scripts/package/linux/appimage/outpost.desktop "$appdir/outpost.desktop"
chmod 755 "$appdir/AppRun"

runtime="${RUNTIME_FILE:-$work/runtime-x86_64}"
if [ -z "${RUNTIME_FILE:-}" ]; then
  curl -fsSL -o "$runtime" "$RUNTIME_URL"
fi
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$runtime" | cut -d' ' -f1)"
else
  actual="$(shasum -a 256 "$runtime" | cut -d' ' -f1)"
fi
if [ "$actual" != "$RUNTIME_SHA256" ]; then
  echo "the AppImage runtime does not match the pinned SHA-256 (got $actual)" >&2
  exit 1
fi

mksquashfs "$appdir" "$work/app.squashfs" -root-owned -noappend -comp gzip -quiet
cat "$runtime" "$work/app.squashfs" > "$OUT"
chmod 755 "$OUT"
echo "Built $OUT ($(wc -c < "$OUT" | tr -d ' ') bytes)"
