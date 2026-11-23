#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
out=assets/ThirdPartyLicenses.html
cargo about generate --config scripts/licenses/about.toml --workspace scripts/licenses/about.hbs -o "$out"
sparkle="$(scripts/fetch-sparkle.sh)/LICENSE"
python3 -I - "$out" "$sparkle" <<'PY'
import html, sys
out, sparkle = sys.argv[1], sys.argv[2]
page = open(out).read()
section = "<h2>Sparkle (MIT)</h2>\n<p>Used by: the in-app updater.</p>\n<pre>" + html.escape(open(sparkle).read()) + "</pre>"
open(out, "w").write(page.replace("<!-- sparkle -->", section))
PY
echo "✓ $out"
