#!/bin/sh
# Adds Outpost to your application menu, for your user only (no root needed).
# Run it from inside the unpacked Outpost folder:   ./install-menu-entry.sh
# To undo it, delete the file it prints at the end.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
entry="$apps/outpost.desktop"

if [ ! -x "$here/bin/Outpost" ]; then
  echo "Run this script from inside the unpacked Outpost folder (bin/Outpost was not found next to it)." >&2
  exit 1
fi

mkdir -p "$apps"
cat > "$entry" <<EOF
[Desktop Entry]
Type=Application
Name=Outpost
Comment=Tor-only BTC/XMR atomic swaps
Exec="$here/bin/Outpost"
Icon=$here/Outpost.png
Terminal=false
Categories=Network;Finance;
EOF
chmod 644 "$entry"

echo "Outpost was added to your application menu."
echo "Menu entry: $entry"
echo "If it does not show up right away, log out and back in, or run: update-desktop-database \"$apps\""
