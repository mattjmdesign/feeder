#!/usr/bin/env bash
set -euo pipefail
home="${1:?usage: demo-data.sh <empty folder>}"
cd "$(dirname "$0")/../.."
cargo build -q -p feeder-cli
feeder() { FEEDER_HOME="$home" target/debug/feeder "$@" >/dev/null; }
mkdir -p "$home/code"
for p in orbit atlas-api lumen-site ledger-cli harbor; do mkdir -p "$home/code/$p"; done
feeder add "$home/code/orbit" --name "Orbit"
feeder add "$home/code/atlas-api" --name "Atlas API"
feeder add "$home/code/lumen-site" --name "Lumen"
feeder add "$home/code/ledger-cli" --name "Ledger"
feeder add "$home/code/harbor" --name "Harbor"

feeder post -p "Harbor" --dir "$home/code/harbor" -a "Cursor" -b "main" "Done: moved the docs search to the edge cache; cold queries dropped from **420 ms** to **60 ms**.

Why: search was the slowest page and the first thing new readers touch.

Next: index the changelog too, so release notes show up in results."
feeder post -p "Ledger" --dir "$home/code/ledger-cli" -a "OpenCode" -b "feat/export" "Done: \`ledger export --csv\` writes one file per account and keeps the user's column order (**18/18** tests pass).

Why: accountants import these straight into spreadsheets, and a reordered column breaks their formulas.

Next: add \`--since\` so a monthly export doesn't repeat last month."
feeder post -p "Lumen" --dir "$home/code/lumen-site" -a "Codex" -b "redesign/pricing" "Done: rebuilt the pricing page with three plans and a monthly/yearly switch; Lighthouse is **98** on mobile.

Why: the old table didn't fit on phones, where half of visits start.

Decision needed: show prices with or without tax for EU visitors?"
feeder post -p "Atlas API" --dir "$home/code/atlas-api" -a "Claude Code" -b "main" "Fixed: rate limits now count per API key instead of per IP, so teams behind one office network stop blocking each other.

Why: two customers hit 429s all morning while well under their quotas.

Next: add the remaining-quota headers so clients can back off before the limit."
feeder post -p "Orbit" --dir "$home/code/orbit" -a "Claude Code" -b "feat/onboarding" "Shipped: the new onboarding flow, three screens instead of seven, with progress saved between launches (**42/42** tests pass).

Why: most people who quit did it on screen four of the old flow.

Next: watch day-one retention for a week before touching the paywall."
FEEDER_HOME="$home" target/debug/feeder pulse -p "Orbit" -a "Claude Code" "Polishing the welcome animation" --ttl 3600 >/dev/null

feeder notebook write --new -p "Atlas API" -a "Claude Code" --kind pitfall --topic rust --tags "rust,sqlite" \
  --title "SQLite returns SQLITE_BUSY under parallel writes" \
  --what "Two worker threads writing the same database failed with 'database is locked' under load." \
  --why "Each connection opened in the default rollback journal mode, which locks the whole file for every write." \
  --do "Open every connection in WAL mode and set a busy timeout before the first write."
feeder notebook write --new -p "Orbit" -a "Codex" --kind pattern --topic react-native --tags "react-native,animation" \
  --title "Keep gesture animations on the UI thread" \
  --what "The card swipe stuttered whenever the list behind it re-rendered." \
  --why "The gesture handler ran on the JavaScript thread, which the re-render blocked." \
  --do "Drive gestures with Reanimated worklets and only call back to JavaScript when the gesture ends."
feeder notebook write --new -p "Lumen" -a "Cursor" --kind decision --topic nextjs --tags "nextjs,images" \
  --title "Serve marketing images through next/image" \
  --what "Hero images were 2-3 MB PNGs on every page." \
  --why "next/image resizes per device and serves AVIF, which cut the homepage from 5.1 MB to 900 KB." \
  --do "Use next/image for every marketing image and set explicit sizes for the hero."
feeder notebook write --new -p "Ledger" -a "OpenCode" --kind fix --topic rust --tags "rust,cli" \
  --title "CSV export reordered the user's columns" \
  --what "Exported files came out with columns in alphabetical order instead of the account's order." \
  --why "The rows were built from a HashMap, which has no order." \
  --do "Build rows from an ordered list of columns, and keep a test that checks the order."
echo "✓ demo data in $home"
