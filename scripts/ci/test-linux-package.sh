#!/usr/bin/env bash
# Tests one Linux package of Outpost on the machine it runs on.
# Usage: test-linux-package.sh <appimage|deb|rpm> <folder with the downloaded files> <label>
# It installs or unpacks the package, starts the app under a virtual screen for 25 seconds,
# saves a screenshot, and (for deb/rpm) removes the package again.
# Annotations (::notice / ::error) say what happened; the exit code is 1 if anything failed.
set -u
kind="${1:?appimage, deb or rpm}"; dir="${2:?folder}"; label="${3:?label}"
here="$(cd "$(dirname "$0")" && pwd)"
shots="${SHOTS:-screenshots}"; mkdir -p "$shots"
SUDO=""; [ "$(id -u)" != 0 ] && SUDO="sudo"
fail=0
note() { echo "::notice title=$label $kind::$*"; }
bad()  { echo "::error title=$label $kind::$*"; fail=1; }

run_app() {  # run_app <screenshot name> <command...>
  local name="$1"; shift
  export JAVA_TOOL_OPTIONS="-Dprism.order=sw"
  local state
  state=$("$here/offline-run.sh" xvfb-run -a --server-args="-screen 0 1280x800x24" "$here/smoke-linux.sh" "$shots/$name.png" "$@" | tail -n 1)
  if [ "$state" = "running" ]; then
    note "the app started and stayed running for 25 seconds (screenshot saved)"
    "$here/check-screen-text.sh" "$shots/$name.png" "$label $kind" || fail=1
  else
    bad "the app $state: $(tail -n 8 "$shots/$name.log" 2>/dev/null | tr '\n' ' ' | cut -c1-400)"
  fi
}

pick() { ls "$dir"/outpost-*."$1" 2>/dev/null | head -n 1; }

case "$kind" in
appimage)
  img="$(pick AppImage)"
  [ -n "$img" ] || { bad "no AppImage found in $dir"; exit 1; }
  chmod +x "$img"
  head -c 4 "$img" | grep -q ELF && note "the AppImage starts with the ELF runtime header" || bad "the AppImage does not start with an ELF header"
  run_app "$label-appimage" "$img" --appimage-extract-and-run
  ;;
deb)
  f="$(pick deb)"
  [ -n "$f" ] || { bad "no .deb found in $dir"; exit 1; }
  f="$(cd "$(dirname "$f")" && pwd)/$(basename "$f")"
  if $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y "$f"; then
    note "the .deb installed"
  else
    bad "the .deb did not install cleanly (details in the log)"
    echo "----- the package's post-install script:"
    cat /var/lib/dpkg/info/outpost.postinst 2>/dev/null || echo "(not found)"
    echo "----- retry after creating the standard menu folders:"
    $SUDO mkdir -p /usr/share/applications /usr/share/desktop-directories /etc/xdg/menus/applications-merged
    if $SUDO env DEBIAN_FRONTEND=noninteractive dpkg --configure -a; then
      note "it configures fine once the menu folders exist, so the failure is specific to minimal systems"
    else
      bad "it still fails after the menu folders were created"
    fi
  fi
  desk="$(ls /usr/share/applications/*utpost*.desktop /opt/outpost/lib/*.desktop 2>/dev/null | head -n 1)"
  [ -n "$desk" ] && note "menu entry file: $desk ($(grep -m1 '^Icon=' "$desk"))" || bad "no menu entry file found after install"
  if [ -x /opt/outpost/bin/Outpost ]; then
    run_app "$label-deb" /opt/outpost/bin/Outpost
  else
    bad "/opt/outpost/bin/Outpost is not there"
  fi
  if $SUDO env DEBIAN_FRONTEND=noninteractive apt-get remove -y outpost >/dev/null 2>&1 && [ ! -e /opt/outpost/bin/Outpost ]; then
    note "the .deb removed cleanly"
  else
    bad "the .deb did not remove cleanly"
  fi
  ;;
rpm)
  f="$(pick rpm)"
  [ -n "$f" ] || { bad "no .rpm found in $dir"; exit 1; }
  f="$(cd "$(dirname "$f")" && pwd)/$(basename "$f")"
  if $SUDO dnf install -y "$f"; then note "the .rpm installed"; else bad "the .rpm did not install"; fi
  desk="$(ls /usr/share/applications/*utpost*.desktop /opt/outpost/lib/*.desktop 2>/dev/null | head -n 1)"
  [ -n "$desk" ] && note "menu entry file: $desk ($(grep -m1 '^Icon=' "$desk"))" || bad "no menu entry file found after install"
  if [ -x /opt/outpost/bin/Outpost ]; then run_app "$label-rpm" /opt/outpost/bin/Outpost; else bad "/opt/outpost/bin/Outpost is not there"; fi
  if $SUDO rpm -e outpost && [ ! -e /opt/outpost/bin/Outpost ]; then note "the .rpm removed cleanly"; else bad "the .rpm did not remove cleanly"; fi
  ;;
*) echo "unknown kind $kind" >&2; exit 2 ;;
esac
exit "$fail"
