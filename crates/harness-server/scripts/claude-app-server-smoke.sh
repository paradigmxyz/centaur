#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/Cargo.toml"
BIN="$ROOT/target/debug/harness-server"

CLAUDE_BIN="${CLAUDE_BIN:-claude}"
MODEL="${CENTAUR_REAL_CLAUDE_MODEL:-${CLAUDE_MODEL:-sonnet}}"
EXPECTED_OUTPUT="${EXPECTED_OUTPUT:-CENTAUR_CLAUDE_APP_SERVER_OK}"
PROMPT="${1:-Reply with exactly: ${EXPECTED_OUTPUT}}"
TIMEOUT_S="${TIMEOUT_S:-180}"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required for this smoke test" >&2
  exit 2
fi

if ! command -v "$CLAUDE_BIN" >/dev/null 2>&1; then
  echo "Claude Code CLI not found: $CLAUDE_BIN" >&2
  echo "Set CLAUDE_BIN=/path/to/claude if it is not on PATH." >&2
  exit 2
fi

tmp_stdout="$(mktemp)"
tmp_stderr="$(mktemp)"
bridge_pid=""

cleanup() {
  if [[ -n "$bridge_pid" ]] && kill -0 "$bridge_pid" >/dev/null 2>&1; then
    kill "$bridge_pid" >/dev/null 2>&1 || true
    wait "$bridge_pid" >/dev/null 2>&1 || true
  fi
  rm -f "$tmp_stdout" "$tmp_stderr"
}
trap cleanup EXIT

echo "Building harness-server..."
cargo build --manifest-path "$MANIFEST" --bin harness-server >/dev/null

echo "Claude Code: $("$CLAUDE_BIN" --version)"
echo "Bridge command: $BIN claude-code"
echo "Claude command inside bridge:"
echo "  $CLAUDE_BIN --print --output-format stream-json --verbose --include-partial-messages --dangerously-skip-permissions --permission-mode bypassPermissions --model $MODEL --session-id <uuid>"
echo

coproc BRIDGE {
  CLAUDE_BIN="$CLAUDE_BIN" "$BIN" claude-code 2>"$tmp_stderr"
}
bridge_pid="$BRIDGE_PID"
bridge_out="${BRIDGE[0]}"
bridge_in="${BRIDGE[1]}"

send_json() {
  local payload="$1"
  echo ">>> $payload"
  printf '%s\n' "$payload" >&"$bridge_in"
}

now_ms() {
  perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000'
}

send_json "$(jq -nc '{id:1, method:"initialize", params:{clientInfo:{name:"bash-smoke", title:null, version:"0"}}}')"
send_json "$(jq -nc --arg model "$MODEL" '{id:2, method:"thread/start", params:{model:$model}}')"

assistant_text=""
thread_id=""
turn_status=""
turn_error=""
sent_turn_start=0
delta_count=0
deadline=$((SECONDS + TIMEOUT_S))
started_ms="$(now_ms)"

while (( SECONDS < deadline )); do
  if ! IFS= read -r -t 1 line <&"$bridge_out"; then
    continue
  fi

  current_ms="$(now_ms)"
  elapsed_ms=$((current_ms - started_ms))
  echo "<<< +${elapsed_ms}ms $line"
  printf '%s\n' "$line" >>"$tmp_stdout"

  if ! jq -e . >/dev/null 2>&1 <<<"$line"; then
    echo "Bridge emitted non-JSON stdout line" >&2
    exit 1
  fi

  id="$(jq -r '.id // empty' <<<"$line")"
  method="$(jq -r '.method // empty' <<<"$line")"

  if [[ "$id" == "2" && "$sent_turn_start" == "0" ]]; then
    thread_id="$(jq -r '.result.thread.id // empty' <<<"$line")"
    if [[ -z "$thread_id" ]]; then
      echo "thread/start did not return result.thread.id" >&2
      exit 1
    fi
    send_json "$(
      jq -nc \
        --arg thread_id "$thread_id" \
        --arg prompt "$PROMPT" \
        '{id:3, method:"turn/start", params:{threadId:$thread_id, input:[{type:"text", text:$prompt}]}}'
    )"
    sent_turn_start=1
  fi

  if [[ "$method" == "item/agentMessage/delta" ]]; then
    delta="$(jq -r '.params.delta // ""' <<<"$line")"
    assistant_text="${assistant_text}${delta}"
    delta_count=$((delta_count + 1))
    echo "--- delta #$delta_count: $(jq -r '.params.delta // ""' <<<"$line" | perl -0pe 's/\n/\\n/g')"
  fi

  if [[ "$method" == "turn/completed" ]]; then
    turn_status="$(jq -r '.params.turn.status // empty' <<<"$line")"
    turn_error="$(jq -r '.params.turn.error.message // empty' <<<"$line")"
    break
  fi
done

eval "exec ${bridge_in}>&-"
wait "$bridge_pid"
bridge_pid=""

if [[ -s "$tmp_stderr" ]]; then
  echo
  echo "Bridge stderr:"
  sed 's/^/!!! /' "$tmp_stderr"
fi

echo
echo "Validating captured stdout against codex_app_server_protocol::JSONRPCMessage / ServerNotification..."
"$BIN" validate-jsonrpc <"$tmp_stdout"

echo
echo "Reconstructed Claude Code output:"
printf '%s\n' "$assistant_text"
echo
echo "Agent message delta events: $delta_count"

if [[ "$turn_status" != "completed" ]]; then
  echo "Expected turn status completed, got '${turn_status:-<none>}'" >&2
  if [[ -n "$turn_error" ]]; then
    echo "Turn error: $turn_error" >&2
  fi
  exit 1
fi

if [[ -n "$EXPECTED_OUTPUT" && "$assistant_text" != *"$EXPECTED_OUTPUT"* ]]; then
  echo "Expected output to contain '$EXPECTED_OUTPUT'" >&2
  exit 1
fi

echo
echo "OK: real Claude Code streamed through the Rust bridge as typed Codex App Server V2 JSON."
