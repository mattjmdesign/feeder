#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

render() {
  echo "# Changelog"
  echo
  echo "What changed in each version of Feeder, newest first. Each section is copied from"
  echo "\`release-notes/<version>.md\` by \`scripts/changelog.sh\`; edit those files, not this one."
  for notes in $(ls release-notes/*.md | sort -t/ -k2 -V -r); do
    version="$(basename "$notes" .md)"
    echo
    echo "## $version"
    echo
    sed '/^## /d' "$notes" | sed '/./,$!d'
  done
}

if [[ "${1:-}" == "--check" ]]; then
  if ! diff -q <(render) CHANGELOG.md >/dev/null 2>&1; then
    echo "CHANGELOG.md is out of date. Run scripts/changelog.sh and commit it." >&2
    exit 1
  fi
  exit 0
fi
render > CHANGELOG.md
