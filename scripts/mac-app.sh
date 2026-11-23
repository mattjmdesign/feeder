#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
case "${1:-}" in
  ""|--no-open|--package) ;;
  *) echo "Usage: $0 [--no-open | --package]" >&2; exit 2 ;;
esac
if [[ $# -gt 1 ]]; then
  echo "Usage: $0 [--no-open | --package]" >&2
  exit 2
fi
MODE="${1:-}"

if [[ -d target ]] && (($(du -sk target | cut -f1) > 8 * 1024 * 1024)); then
  echo "› target/ is over 8 GB; clearing it"
  find target -mindepth 1 -maxdepth 1 ! -name sparkle ! -name CACHEDIR.TAG -exec rm -rf {} +
fi

APP_NAME="Feeder"
BUNDLE_ID="com.mattjm.feeder"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
MACOS_MIN="13.0"
RUST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
case "$RUST_TARGET" in
  aarch64-apple-darwin) ARCH="arm64" ;;
  x86_64-apple-darwin) ARCH="x86_64" ;;
  *) echo "Building Feeder.app requires a macOS Rust toolchain." >&2; exit 1 ;;
esac
RELEASE_DIR="target/$RUST_TARGET/release"
DEST="$HOME/Applications/$APP_NAME.app"
if [[ "$MODE" != "--package" && -z "${FEEDER_REPLACE_RELEASE:-}" ]] \
  && /usr/libexec/PlistBuddy -c 'Print :SUFeedURL' "$DEST/Contents/Info.plist" >/dev/null 2>&1; then
  echo "$DEST is a release build that updates itself; a local build would replace it and its updater." >&2
  echo "Use 'cargo run -p feeder-app' to try changes, or set FEEDER_REPLACE_RELEASE=1 to replace it anyway." >&2
  exit 1
fi
SIGN_IDENTITY="${FEEDER_SIGN_IDENTITY:--}"
NOTARY_PROFILE="${FEEDER_NOTARY_PROFILE:-}"
UPDATE_FEED="${FEEDER_UPDATE_FEED_URL:-}"
UPDATE_KEY="${FEEDER_UPDATE_PUBLIC_KEY:-}"
if [[ -n "$NOTARY_PROFILE" && ( "$MODE" != "--package" || "$SIGN_IDENTITY" == "-" ) ]]; then
  echo "Notarization requires --package and a Developer ID Application signing identity." >&2
  exit 2
fi
if [[ -n "$UPDATE_FEED" || -n "$UPDATE_KEY" || -n "$NOTARY_PROFILE" ]]; then
  if [[ "$UPDATE_FEED" != https://?* || ! "$UPDATE_KEY" =~ ^[A-Za-z0-9+/]{43}=$ ]]; then
    echo "Updates require an HTTPS FEEDER_UPDATE_FEED_URL and a Sparkle FEEDER_UPDATE_PUBLIC_KEY." >&2
    exit 2
  fi
fi

SPARKLE_DIR=""
if [[ -n "$UPDATE_FEED" ]]; then
  SPARKLE_DIR="$(scripts/fetch-sparkle.sh)"
fi

if [[ "$MODE" == "--package" ]]; then
  mkdir -p dist
  WORK="$(mktemp -d "$PWD/dist/.feeder-package.XXXXXX")"
else
  WORK="$(mktemp -d)"
fi
trap 'rm -rf "$WORK"' EXIT
STAGE="$WORK/$APP_NAME.app"

echo "› building (release)"
FLAG_SEPARATOR=$'\x1f'
RELEASE_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS-${RUSTFLAGS:-}}"
if [[ -z "${CARGO_ENCODED_RUSTFLAGS+x}" ]]; then
  RELEASE_RUSTFLAGS="${RELEASE_RUSTFLAGS// /$FLAG_SEPARATOR}"
fi
RELEASE_RUSTFLAGS+="${RELEASE_RUSTFLAGS:+$FLAG_SEPARATOR}--remap-path-prefix=$HOME/=/build/home/"
RELEASE_RUSTFLAGS+="$FLAG_SEPARATOR--remap-path-prefix=$PWD/=/build/feeder/"
MACOSX_DEPLOYMENT_TARGET="$MACOS_MIN" FEEDER_SPARKLE_DIR="$SPARKLE_DIR" CARGO_ENCODED_RUSTFLAGS="$RELEASE_RUSTFLAGS" \
  cargo build --locked --release --target-dir target --target "$RUST_TARGET" -p feeder-app -p feeder-cli
if LC_ALL=C /usr/bin/grep -aFq -e "$HOME/" -e "$PWD/" "$RELEASE_DIR/feeder-app" "$RELEASE_DIR/feeder"; then
  echo "The release binaries still contain an absolute path from the build Mac." >&2
  exit 1
fi

echo "› rendering the icon"
env -u FEEDER_SPARKLE_DIR cargo build --locked --target-dir target -p feeder-app
ICONSET="$WORK/AppIcon.iconset"
mkdir -p "$ICONSET"
RAW="$WORK/icon-1024.png"
FEEDER_HOME="$WORK/icon-data" target/debug/feeder-app --snap "$RAW" --scene app-icon --size 1024x1024 --scale 1 >/dev/null
for size in 16 32 128 256 512; do
  sips -z $size $size "$RAW" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) "$RAW" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$ICONSET/../AppIcon.icns"

echo "› assembling $APP_NAME.app"
mkdir -p "$STAGE/Contents/MacOS" "$STAGE/Contents/Resources" "$STAGE/Contents/Helpers"
cp "$RELEASE_DIR/feeder-app" "$STAGE/Contents/MacOS/$APP_NAME"
cp "$RELEASE_DIR/feeder" "$STAGE/Contents/Helpers/feeder"
cp "$ICONSET/../AppIcon.icns" "$STAGE/Contents/Resources/AppIcon.icns"
cp assets/Credits.html "$STAGE/Contents/Resources/Credits.html"
cp assets/ThirdPartyLicenses.html "$STAGE/Contents/Resources/ThirdPartyLicenses.html"
cat > "$STAGE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleExecutable</key><string>$APP_NAME</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>$MACOS_MIN</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHumanReadableCopyright</key><string>Copyright © 2026 Matt Mitchell. MIT License.</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>$BUNDLE_ID</string>
      <key>CFBundleURLSchemes</key>
      <array>
        <string>feeder</string>
      </array>
    </dict>
  </array>
</dict>
</plist>
PLIST
if [[ -n "$SPARKLE_DIR" ]]; then
  INFO="$STAGE/Contents/Info.plist"
  plutil -insert SUFeedURL -string "$UPDATE_FEED" "$INFO"
  plutil -insert SUPublicEDKey -string "$UPDATE_KEY" "$INFO"
  plutil -insert SUEnableAutomaticChecks -bool YES "$INFO"
  plutil -insert SUScheduledCheckInterval -integer 3600 "$INFO"
  plutil -insert SUAllowsAutomaticUpdates -bool NO "$INFO"
  plutil -insert SUVerifyUpdateBeforeExtraction -bool YES "$INFO"
  plutil -insert SURequireSignedFeed -bool YES "$INFO"
fi

plutil -lint "$STAGE/Contents/Info.plist" >/dev/null
SIGN_ARGS=(--force --sign "$SIGN_IDENTITY")
if [[ "$SIGN_IDENTITY" != "-" ]]; then
  SIGN_ARGS+=(--options runtime --timestamp)
fi
if [[ -n "$SPARKLE_DIR" ]]; then
  FRAMEWORK="$STAGE/Contents/Frameworks/Sparkle.framework"
  mkdir -p "$(dirname "$FRAMEWORK")"
  ditto "$SPARKLE_DIR/Sparkle.framework" "$FRAMEWORK"
  rm -rf "$FRAMEWORK/Versions/Current/XPCServices"
  rm -f "$FRAMEWORK/XPCServices"
  codesign "${SIGN_ARGS[@]}" --preserve-metadata=entitlements "$FRAMEWORK/Versions/Current/Autoupdate"
  codesign "${SIGN_ARGS[@]}" --preserve-metadata=entitlements "$FRAMEWORK/Versions/Current/Updater.app"
  codesign "${SIGN_ARGS[@]}" "$FRAMEWORK"
fi
codesign "${SIGN_ARGS[@]}" "$STAGE/Contents/Helpers/feeder"
codesign "${SIGN_ARGS[@]}" "$STAGE"
codesign --verify --deep --strict "$STAGE"

if [[ "$MODE" == "--package" ]]; then
  ARCHIVE="$APP_NAME-$VERSION-macos-$ARCH-local"
  if [[ -n "$NOTARY_PROFILE" ]]; then
    ARCHIVE="$APP_NAME-$VERSION-macos-$ARCH"
  fi

  DMG="$WORK/$ARCHIVE.dmg"
  if command -v uvx >/dev/null; then
    uvx --quiet --from 'dmgbuild==1.6.5' dmgbuild -s scripts/dmg/settings.py \
      -D app="$STAGE" -D background=scripts/dmg/background.tiff -D volume_icon="$STAGE/Contents/Resources/AppIcon.icns" \
      "$APP_NAME" "$DMG" >/dev/null
  else
    echo "› uvx not found; building a plain disk image" >&2
    mkdir "$WORK/image"
    mv "$STAGE" "$WORK/image/"
    ln -s /Applications "$WORK/image/Applications"
    hdiutil create -quiet -volname "$APP_NAME" -format UDZO -srcfolder "$WORK/image" "$DMG"
  fi

  if [[ -n "$NOTARY_PROFILE" ]]; then
    echo "› signing and notarizing $ARCHIVE.dmg"
    codesign --force --sign "$SIGN_IDENTITY" --timestamp --identifier "$BUNDLE_ID.dmg" "$DMG"
    mv -f "$DMG" "dist/$ARCHIVE.dmg"
    DMG="dist/$ARCHIVE.dmg"
    xcrun notarytool submit "$DMG" --wait --keychain-profile "$NOTARY_PROFILE"
    xcrun stapler staple "$DMG"
    xcrun stapler validate "$DMG"
    spctl --assess --type open --context context:primary-signature "$DMG"
  else
    app_src="$STAGE"
    [[ -d "$app_src" ]] || app_src="$WORK/image/$APP_NAME.app"
    ditto -c -k --keepParent "$app_src" "$WORK/$ARCHIVE.zip"
    mv -f "$WORK/$ARCHIVE.zip" "dist/$ARCHIVE.zip"
    mv -f "$DMG" "dist/$ARCHIVE.dmg"
  fi
  echo "✓ dist/$ARCHIVE.dmg"
  exit 0
fi

echo "› installing to $DEST"
osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
mkdir -p "$HOME/Applications"
rm -rf "$DEST"
mv "$STAGE" "$DEST"
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$DEST" >/dev/null 2>&1 || true

echo "› installing the feeder CLI"
mkdir -p "$HOME/.cargo/bin"
rm -f "$HOME/.cargo/bin/feeder"
ln -s "$DEST/Contents/Helpers/feeder" "$HOME/.cargo/bin/feeder"

if [[ "$MODE" != "--no-open" ]]; then
  sleep 0.5
  open "$DEST"
fi
echo "✓ $APP_NAME.app installed"
