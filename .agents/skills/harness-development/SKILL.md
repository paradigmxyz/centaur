---
name: harness-development
description: "Add, modify, or debug Centaur harness-server backends in crates/harness-server. Use when adding a new harness CLI, changing how Codex, Claude Code, Amp, Pi, Nanocodex, or Hermes output is normalized, or investigating harness streaming, interrupt, resume, model, or reasoning-effort behavior."
---

# Harness Development

## Shape of the crate

`crates/harness-server` is the only layer that translates harness output. It is
built into the sandbox image and is the sandbox's PID 1: api-rs launches
`harness-server <subcommand>` (`harness_server_subcommand` in
`services/api-rs/crates/centaur-session-runtime/src/lib.rs`). Don't add harness
output parsing in Python, TypeScript, api-rs, or the ingresses.

- **Input:** By default (`--mode blocks`) stdin carries Centaur blocks NDJSON:
  `user` (text or content blocks, with optional `model`, `provider`,
  `reasoning`, and trace context), `attachment.chunk`, and `interrupt`. See
  `parse_blocks_line_with_state` in `src/server.rs`. `--mode jsonrpc` accepts
  Codex App Server requests instead, including `turn/steer`. It is used for
  protocol testing.
- **Output:** stdout carries only Codex App Server V2 notifications, typed with
  the pinned `codex-app-server-protocol` crate (`thread/started`,
  `item/*`, `turn/completed`, `error`). Harness stderr may be logged, but
  nothing else may reach stdout.

Backends:

- `codex.rs`: runs `codex app-server` and passes its native protocol through.
- `claude.rs`, `amp.rs`, `pi.rs`: implement `HarnessServer` (`src/traits.rs`)
  on the shared runner in `server.rs`. Each one builds the process command and
  stdin lines, parses stdout lines into typed events, and normalizes them into
  `NormalizedEvent`. `CodexTurnNormalizer` (`turn.rs`) turns those into App
  Server items.
- `nanocodex.rs`, `hermes.rs`: their own blocks servers (an in-process library
  and a long-lived JSON-RPC gateway). They also feed `CodexTurnNormalizer`.

## Adding or changing a backend

1. **Observe the native CLI first.** Start from the args in the backend's
   `command_for_turn` (or the vendor docs for a new harness). Feed it
   hand-written stdin, and save every stdin, stdout, and stderr line to a
   temp directory. Establish the real contract: startup args, input shape,
   event types, terminal event, session id and resume, tool-call and tool-result
   shape, interrupt, model switching, and reasoning controls.
2. **Implement `HarnessServer`** in one module, `src/<harness>.rs`. Use typed
   `serde` event enums. Use `serde_json::Value` only at the parser boundary.
   Override the trait hooks only when the harness needs them:
   `terminal_assistant_stop_settle`, `turn_hold`, `restart_on_model_change`,
   `validate_model`, `reasoning_effort`, and `stdin_for_reasoning_effort`.
3. **Wire it up.** Add the subcommand in `src/main.rs`, the `HarnessKind`
   variant, and dispatch in `src/server.rs`. The binary path should be
   overridable through an env var (`CLAUDE_BIN`, `AMP_BIN`, `CENTAUR_PI_BIN`)
   so tests can substitute a fake. Namespace new settings as `CENTAUR_<HARNESS>_*`.
4. **For a new harness, update the rest of the path.** `src/pi.rs` and commit
   `8814c7f8` are the reference:
   - api-rs `HarnessType` (`centaur-session-core`), the subcommand mapping, and
     auth mode / proxy wiring (`centaur-api-server/src/args.rs`).
   - Install the CLI and persist its state in `services/sandbox/Dockerfile` and
     `entrypoint.sh`.
   - Add the selector in the ingresses (`services/slackbotv2/src/overrides.ts`
     and `response-context.ts`) and in the Console harness list.
   - Add a page under `docs/pages/extend/` and update
     `docs/pages/reference/configuration.mdx`.

## Invariants

- Keep one harness process per thread across turns, and resume it with the
  native session id (`--resume`, `threads continue`, `--session-id`). Never
  silently start a fresh conversation when the caller expects continuity.
- Every turn emits the `userMessage` item started and completed events and ends
  with exactly one `turn/completed` that includes all of the turn's items.
- Complete a turn only at the harness's real terminal boundary: Claude Code's
  `result`, Pi's `agent_settled`, or Amp's terminal assistant stop, since Amp may
  not emit `result` while stdin is open. Use `terminal_assistant_stop_settle`
  rather than ad hoc timers.
- Interrupt is cancellation, and it is distinct from steering. Codex sends
  `turn/interrupt` to the app server. The shared runner kills the harness
  process and finishes the turn as interrupted, and the next turn respawns the
  process and resumes the session.
- Reject an unsupported model with a turn error worded "unsupported model".
  Slack keys on that wording to clear the thread's sticky model.
- Never log or emit credentials. Sandboxes see iron-proxy placeholders. Don't
  inject real keys in harness-server.

## Tests

From `crates/harness-server`, run what CI runs:

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

- **Unit tests** in each module: stdin generation, stdout parsing, and event
  normalization for representative recorded lines.
- **Stdio integration tests** (`tests/app_server_stdio.rs`, `tests/pi_stdio.rs`):
  run the real `harness-server` binary against a scripted fake harness that
  replays recorded native output from `tests/fixtures/<harness>/*.jsonl`. Record
  fixtures from the real CLI and note the CLI version in the test. Assert on the
  emitted App Server stream: item order, phases, `turn/completed`, interrupt,
  resume args, and model switches.
- **Real-binary tests** are `#[ignore]` tests that spawn installed harnesses
  and make network calls. Run them when native behavior is the question:

```bash
cargo test --manifest-path crates/harness-server/Cargo.toml \
  real_harnesses_basic_steer_and_resume -- --ignored --nocapture
```

Other real tests in `tests/app_server_stdio.rs` include
`real_claude_code_long_streaming_is_anchored_to_native_cli`,
`real_amp_long_streaming_is_anchored_to_native_cli`, and
`real_codex_long_streaming_uses_native_app_server_chunks`. Read the
`--nocapture` output, not just the summary. Look for non-JSON stdout, missing
`item/completed`, duplicate or stale assistant text, wrong thread or turn ids,
and lost session continuity.

For changes that affect the deployed path, also build the sandbox image and run
a real turn through the local stack. `e2e/stack.sh test` covers the Slack path
with scripted model providers.

Record any native quirk in the backend module or its tests, not only in the PR
description.
