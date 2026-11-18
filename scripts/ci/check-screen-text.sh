#!/usr/bin/env bash
# Reads the text on a screenshot of the running app and checks it shows Outpost's first screen.
# Usage: check-screen-text.sh <screenshot.png> <label>
# A process that is merely "still running" proves nothing (an error box keeps it alive), so this checks what is on screen:
# a fresh profile must show "Connect wallet" (or "Create new wallet"), and an error box must not be there.
# Exit 0 = the first screen is visible; exit 1 = it is not; exit 0 with a warning = the check could not run (no OCR tool).
set -u
shot="${1:?screenshot}"; label="${2:-screen}"
if ! command -v tesseract >/dev/null 2>&1; then
  echo "::warning title=$label screen::tesseract is not installed, so the text on the screen was not checked"
  exit 0
fi
if [ ! -s "$shot" ]; then
  echo "::error title=$label screen::there is no screenshot at $shot, so nothing can be shown to be on screen"
  exit 1
fi
# The screen is light text on a dark background and older Tesseract versions (4.x, as on Ubuntu 22.04) misread that,
# so the screenshot is also read inverted and enlarged. Any reading that shows the first screen is enough.
variants=("$shot")
im=""
if command -v magick >/dev/null 2>&1; then im="magick"; elif command -v convert >/dev/null 2>&1; then im="convert"; fi
tmp="$(mktemp -d)"
if [ -n "$im" ]; then
  "$im" "$shot" -negate "$tmp/negated.png" >/dev/null 2>&1 && variants+=("$tmp/negated.png")
  "$im" "$shot" -colorspace Gray -negate -resize 300% "$tmp/enlarged.png" >/dev/null 2>&1 && variants+=("$tmp/enlarged.png")
fi
text=""
for v in "${variants[@]}"; do
  text="$text $(tesseract "$v" - --psm 11 2>/dev/null | tr '\n' ' ')"
done
text="$(printf '%s' "$text" | tr -s ' ')"
low="$(printf '%s' "$text" | tr 'A-Z' 'a-z')"
if printf '%s' "$low" | grep -q -E "failed to launch|could not find or load|unable to launch|has stopped working"; then
  echo "::error title=$label screen::the screen shows an error message: $(printf '%s' "$text" | cut -c1-200)"
  exit 1
fi
if printf '%s' "$low" | grep -q -E "connect wallet|create new wallet"; then
  echo "::notice title=$label screen::the first screen is visible (read from the screenshot: $(printf '%s' "$text" | cut -c1-80))"
  exit 0
fi
echo "::error title=$label screen::the Outpost first screen is NOT on the screen. Text read: $(printf '%s' "$text" | cut -c1-200)"
exit 1
