#!/usr/bin/env bash
# Bundle every PNG + SVG in docs/public/brand/ into a single zip that the
# brand page (and right-click menu on the header logo) can hand to visitors.
# The output lives at docs/public/centaur-brand-assets.zip and is regenerated
# on every prebuild so it's always in sync with the source assets.

set -euo pipefail
cd "$(dirname "$0")/.."

OUT=public/centaur-brand-assets.zip
SRC=public/brand

rm -f "$OUT"
cd "$SRC"
zip -q -r "../$(basename "$OUT")" . -i '*.png' '*.svg'
cd - >/dev/null

echo "Built $OUT ($(du -h "$OUT" | cut -f1)) with $(zipinfo -1 "$OUT" | wc -l | tr -d ' ') files"
