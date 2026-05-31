---
name: harness-development
description: "Add, modify, or debug Centaur harness-server backends in crates/harness-server. Use when adding support for a new harness CLI, changing Codex App Server V2 normalization, investigating Claude Code/Amp/Codex streaming or steering behavior, removing Python/TypeScript harness normalizers, or differentially testing real harness stdout/stderr against the shared App Server protocol."
---

# Harness Development

## Overview

Work in `crates/harness-server`. The Rust binary is the only normalization layer for sandbox harness output; do not add Python or TypeScript normalizers, and do not reintroduce per-client protocol shims outside this crate.

The target wire protocol is OpenAI Codex App Server V2. Prefer the pinned `codex-app-server-protocol` Rust types already in `Cargo.toml`; if a type is missing, add a small typed wrapper in Rust rather than passing unstructured JSON through the system.

## Implementation Workflow

1. Observe the native harness CLI before changing the wrapper. Run the real CLI with streaming stdin/stdout, feed hand-written NDJSON, and capture both stdout and stderr.
2. Identify the real process contract: startup args, stdin message shape, stdout event types, terminal event, session id, resume flag or id, multi-turn behavior, tool-use/tool-result shape, and steering behavior.
3. Add one module under `src/` for the backend, such as `src/<harness>.rs`, and implement `HarnessServer` from `src/traits.rs`.
4. Keep conversions inside that harness implementation. Prefer typed `serde` event enums plus explicit `From`/conversion helpers into `NormalizedEvent`; avoid generic `serde_json::Value` plumbing unless it is only at the parser boundary.
5. Wire the subcommand in `src/main.rs` and the dispatch in `src/lib.rs`/`src/server.rs`. The public CLI shape should stay `harness-server codex|claude-code|amp|<new-harness>`.
6. Add unit tests for stdin generation, steering generation, parser behavior, and representative event conversion. Add or extend a real-harness script when native behavior can only be proven with the CLI.

## Protocol Invariants

- The wrapper process stays alive across turns. Do not spawn the underlying harness once per user turn unless the native harness cannot support a live streaming process.
- `turn/start` and `turn/steer` must emit Codex V2 `userMessage` item started and completed events, then include those user-message items in the final `turn/completed` item list.
- Complete a turn only at the harness's real completion boundary. Claude Code completes on its `result` event. Amp's streaming process may not emit `result` until stdin closes, so complete live turns on assistant `end_turn` when that is the observed terminal boundary.
- Do not map steering to interruption. Steering appends a new user message to the active turn; interruption is cancellation and has different semantics.
- Claude Code steering uses another streaming user input message. Amp steering uses a streaming user input message with top-level `steer: true`. Codex uses App Server `turn/steer` natively.
- Resume must preserve the native session id or native resume token and must not silently create a fresh conversation when the caller expects continuity.
- Stdout from `harness-server` must be JSON-RPC/App Server JSON only. Harness stderr can be logged, but raw non-protocol lines must not leak on stdout.

## Native Probing

Use direct native probes when behavior is unclear. Save every stdin line and stdout/stderr line to a temp directory so the wrapped behavior can be compared later.

Claude Code streaming:

```bash
claude --print \
  --input-format stream-json \
  --output-format stream-json \
  --verbose \
  --include-partial-messages \
  --dangerously-skip-permissions \
  --permission-mode bypassPermissions \
  --model "${CENTAUR_REAL_CLAUDE_MODEL:-sonnet}" \
  --session-id "$(uuidgen | tr 'A-Z' 'a-z')"
```

Amp streaming:

```bash
amp --no-ide \
  --no-notifications \
  --no-color \
  --dangerously-allow-all \
  --execute \
  --stream-json \
  --stream-json-input \
  --stream-json-thinking \
  --mode "${AMP_MODE:-smart}"
```

For steering probes, start a long-running tool call, then send the native steering line before the tool finishes. Claude Code should receive a second `{"type":"user","message":...}` line. Amp should receive the same shape with top-level `"steer":true`.

## Differential Test Commands

Run Rust tests first:

```bash
cargo test --manifest-path crates/harness-server/Cargo.toml
```

Run real-harness comparisons from the repo root. Set `BUILD=0` after building once if you are iterating on scripts or prompts only.

```bash
BUILD=0 HARNESSES='claude-code amp codex' \
  TIMEOUT_S=240 READ_TIMEOUT_S=10 \
  crates/harness-server/scripts/compare-real-harnesses.sh
```

```bash
BUILD=0 HARNESSES='claude-code amp codex' \
  TIMEOUT_S=300 READ_TIMEOUT_S=10 \
  crates/harness-server/scripts/compare-steer-real-harnesses.sh
```

```bash
BUILD=0 HARNESSES='claude-code amp codex' \
  TIMEOUT_S=300 READ_TIMEOUT_S=10 \
  crates/harness-server/scripts/compare-multiturn-real-harnesses.sh
```

Validate captured wrapper stdout with:

```bash
target/debug/harness-server validate-jsonrpc < /path/to/stdout.log
```

Inspect the raw logs, not only script summaries. Look for non-JSON stdout, missing `item/completed`, stale final answers after steering, wrong thread or turn ids, lost session continuity on resume, duplicate assistant text, queued steer messages, and process restarts between turns.

## Done Criteria

Consider a harness change done only when:

- Unit tests pass.
- Real Claude Code, Amp, and Codex pass basic, steering, and multi-turn/resume differential scripts unless the change is explicitly scoped to fewer harnesses.
- The logs show the exact stdout JSON-RPC stream and the native harness stderr/stdout observations explain any harness-specific branch.
- Python and TypeScript contain no custom harness output normalization for the changed path.
- Any native quirk is captured in the harness module or tests, not as tribal knowledge in the final response.
