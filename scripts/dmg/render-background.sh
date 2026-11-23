#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
chrome="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
for scale in 1 2; do
  "$chrome" --headless=new --disable-gpu --hide-scrollbars --user-data-dir="$work/profile-$scale" \
    --default-background-color=00000000 --force-device-scale-factor=$scale --window-size=600,380 \
    --screenshot="$work/bg-$scale.png" "file://$PWD/background.html" >/dev/null 2>&1 &
  pid=$!
  for _ in {1..30}; do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
  kill "$pid" 2>/dev/null || true
done
tiffutil -cathidpicheck "$work/bg-1.png" "$work/bg-2.png" -out background.tiff >/dev/null
echo "✓ scripts/dmg/background.tiff"
