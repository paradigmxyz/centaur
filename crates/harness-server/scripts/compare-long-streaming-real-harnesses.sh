#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

PROMPT="${1:-}"
if [[ -z "$PROMPT" ]]; then
  PROMPT="Write exactly 24 lines. Each line must be 'LONG_STREAM_DELTA_LINE_N: abcdefghijklmnopqrstuvwxyz 0123456789' where N is 01 through 24. Do not use markdown, bullets, tools, or extra text."
fi

export HARNESSES="${HARNESSES:-claude-code amp codex}"
export TIMEOUT_S="${TIMEOUT_S:-360}"
export READ_TIMEOUT_S="${READ_TIMEOUT_S:-10}"
export PARALLEL="${PARALLEL:-0}"

exec "$ROOT/scripts/compare-real-harnesses.sh" "$PROMPT"
