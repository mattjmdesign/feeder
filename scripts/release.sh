#!/usr/bin/env bash
# Publishes the committed version of Feeder: builds a signed, notarized DMG, signs an update feed
# for it, and attaches both to a GitHub release on this repo. Installed copies of Feeder check the
# latest release's appcast.xml and offer the update; the website's Download button fetches the
# latest release's Feeder.dmg.
#   scripts/release.sh                 notes from release-notes/<version>.md
#   scripts/release.sh path/to/NOTES.md
# Needs, in this Mac's keychain: a Developer ID Application certificate, the `feeder` notarytool
# profile and the `feeder` Sparkle key. Needs `gh` signed in with access to the releases repo.
set -euo pipefail

cd "$(dirname "$0")/.."
RELEASES_REPO="mattjmdesign/feeder"
# Built into every copy of Feeder. Changing either strands the copies already installed.
FEED_URL="https://github.com/$RELEASES_REPO/releases/latest/download/appcast.xml"
# Feeder 0.2 and 0.3 read their feed from this repo. Every release also posts its signed feed
# there, so those copies are offered the update and pick up FEED_URL once they install it.
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
# notarytool keeps its credential in the data-protection keychain, which macOS seals while the
# screen is locked. Check before a three-minute build rather than after it.
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

case "$(rustc -vV | sed -n 's/^host: //p')" in
  aarch64-apple-darwin) ARCH="arm64" ;;
  *) ARCH="x86_64" ;;
esac
BUILT="dist/Feeder-$VERSION-macos-$ARCH.dmg"
# Every release's image is Feeder.dmg, so releases/latest/download/Feeder.dmg always works; the
# feed tells versions apart by their release tag.
NAME="Feeder"
STAGE="dist/release-$VERSION"
rm -rf "$STAGE"
mkdir -p "$STAGE"
mv "$BUILT" "$STAGE/$NAME.dmg"
# generate_appcast embeds the notes that share the archive's name.
cp "$NOTES" "$STAGE/$NAME.md"

echo "› signing the update feed"
"$(scripts/fetch-sparkle.sh)/bin/generate_appcast" --account feeder --embed-release-notes --maximum-deltas 0 \
  --download-url-prefix "https://github.com/$RELEASES_REPO/releases/download/$TAG/" "$STAGE"

echo "› publishing $TAG"
gh release create "$TAG" --repo "$RELEASES_REPO" --title "Feeder $VERSION" --notes-file "$NOTES" \
  "$STAGE/$NAME.dmg" "$STAGE/appcast.xml"
# The feed's download links point at $RELEASES_REPO, so this copy only redirects older installs.
gh release create "$TAG" --repo "$LEGACY_FEED_REPO" --title "Feeder $VERSION" \
  --notes "Feeder now lives at https://github.com/$RELEASES_REPO. This release only carries the update feed." \
  "$STAGE/appcast.xml"
# Published images live on the releases page; keep only this one locally.
find dist -mindepth 1 -maxdepth 1 ! -name "release-$VERSION" -exec rm -rf {} +
echo "✓ https://github.com/$RELEASES_REPO/releases/tag/$TAG"
