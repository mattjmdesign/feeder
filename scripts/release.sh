#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
RELEASES_REPO="mattjmdesign/feeder"
FEED_URL="https://github.com/$RELEASES_REPO/releases/latest/download/appcast.xml"
LEGACY_FEED_REPO="mattjmdesign/feeder-releases"
PUBLIC_KEY="woqv+TW3eu0pE9safKDR37DCbBMSAvq/IjYQfLFdnm8="

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
TAG="v$VERSION"
NOTES="${1:-release-notes/$VERSION.md}"
if [[ ! -f "$NOTES" ]]; then
  echo "No release notes at $NOTES. Write what changed for people under a '## What's new' heading." >&2
  exit 2
fi
if ! scripts/changelog.sh --check; then
  exit 1
fi
case "$(rustc -vV | sed -n 's/^host: //p')" in
  aarch64-apple-darwin) ARCH="arm64" ;;
  *)
    echo "Release builds run on Apple silicon (aarch64-apple-darwin) only." >&2
    exit 1
    ;;
esac
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$BRANCH" != "main" ]]; then
  echo "Switch to main before releasing (currently on $BRANCH)." >&2
  exit 1
fi
git fetch -q origin main
if [[ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]]; then
  echo "Local main does not match origin/main. Push first." >&2
  exit 1
fi
if ! gh auth status >/dev/null 2>&1; then
  echo "GitHub CLI is not signed in. Run 'gh auth login' first." >&2
  exit 1
fi
if [[ -n "$(git status --porcelain)" ]]; then
  echo "Commit your changes first; a release builds what's committed." >&2
  exit 1
fi
for repo in "$RELEASES_REPO" "$LEGACY_FEED_REPO"; do
  if gh release view "$TAG" --repo "$repo" >/dev/null 2>&1; then
    echo "$TAG is already released on $repo. Bump the version in Cargo.toml first." >&2
    exit 1
  fi
done
if ! xcrun notarytool history --keychain-profile feeder >/dev/null 2>&1; then
  echo "Can't read the 'feeder' notarization credential. Unlock this Mac (or run" >&2
  echo "'xcrun notarytool store-credentials feeder') and try again." >&2
  exit 1
fi
IDENTITY="$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application: .*\)"$/\1/p' | head -1)"
if [[ -z "$IDENTITY" ]]; then
  echo "No Developer ID Application certificate in the keychain." >&2
  exit 1
fi

FEEDER_SIGN_IDENTITY="$IDENTITY" FEEDER_NOTARY_PROFILE=feeder FEEDER_UPDATE_FEED_URL="$FEED_URL" \
  FEEDER_UPDATE_PUBLIC_KEY="$PUBLIC_KEY" scripts/mac-app.sh --package

BUILT="dist/Feeder-$VERSION-macos-$ARCH.dmg"
NAME="Feeder"
STAGE="dist/release-$VERSION"
rm -rf "$STAGE"
mkdir -p "$STAGE"
mv "$BUILT" "$STAGE/$NAME.dmg"
cp "$NOTES" "$STAGE/$NAME.md"

echo "› signing the update feed"
"$(scripts/fetch-sparkle.sh)/bin/generate_appcast" --account feeder --embed-release-notes --maximum-deltas 0 \
  --download-url-prefix "https://github.com/$RELEASES_REPO/releases/download/$TAG/" "$STAGE"

echo "› publishing $TAG"
gh release create "$TAG" --repo "$RELEASES_REPO" --title "Feeder $VERSION" --notes-file "$NOTES" \
  "$STAGE/$NAME.dmg" "$STAGE/appcast.xml"
gh release create "$TAG" --repo "$LEGACY_FEED_REPO" --title "Feeder $VERSION" \
  --notes "Feeder now lives at https://github.com/$RELEASES_REPO. This release only carries the update feed." \
  "$STAGE/appcast.xml"
find dist -mindepth 1 -maxdepth 1 ! -name "release-$VERSION" -exec rm -rf {} +
echo "✓ https://github.com/$RELEASES_REPO/releases/tag/$TAG"
