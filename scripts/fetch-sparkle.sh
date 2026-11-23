#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
VERSION="2.10.1"
SHA256="1a856d2a9edb8e628de88121d7be04cf0e2167f8641650b78fca00b14244cf27"
CACHE="$PWD/target/sparkle"
DEST="$CACHE/$VERSION"

if [[ ! -x "$DEST/bin/generate_appcast" || ! -f "$DEST/Sparkle.framework/Sparkle" ]]; then
  mkdir -p "$CACHE"
  WORK="$(mktemp -d "$CACHE/.download.XXXXXX")"
  trap 'rm -rf "$WORK"' EXIT
  curl --fail --silent --show-error --location \
    "https://github.com/sparkle-project/Sparkle/releases/download/$VERSION/Sparkle-$VERSION.tar.xz" \
    -o "$WORK/Sparkle.tar.xz"
  printf '%s  %s\n' "$SHA256" "$WORK/Sparkle.tar.xz" | shasum -a 256 -c >&2
  mkdir "$WORK/distribution"
  tar -xf "$WORK/Sparkle.tar.xz" -C "$WORK/distribution"
  mv -n "$WORK/distribution" "$DEST"
fi

printf '%s\n' "$DEST"
