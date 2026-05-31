#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/Cargo.toml"
BIN="$ROOT/target/debug/harness-server"

AMP_BIN="${AMP_BIN:-amp}"
MODEL="${AMP_MODE:-deep}"
EXPECTED_OUTPUT="${EXPECTED_OUTPUT:-CENTAUR_AMP_APP_SERVER_OK}"
PROMPT="${1:-Reply with exactly: ${EXPECTED_OUTPUT}}"
TIMEOUT_S="${TIMEOUT_S:-180}"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required for this smoke test" >&2
  exit 2
fi

if ! command -v "$AMP_BIN" >/dev/null 2>&1; then
  echo "Amp CLI not found: $AMP_BIN" >&2
  echo "Set AMP_BIN=/path/to/amp if it is not on PATH." >&2
  exit 2
fi

tmp_stdout="$(mktemp)"
tmp_stderr="$(mktemp)"
server_pid=""

cleanup() {
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" >/dev/null 2>&1; then
    kill "$server_pid" >/dev/null 2>&1 || true
    wait "$server_pid" >/dev/null 2>&1 || true
  fi
  rm -f "$tmp_stdout" "$tmp_stderr"
}
trap cleanup EXIT

echo "Building harness-server..."
cargo build --manifest-path "$MANIFEST" --bin harness-server >/dev/null

echo "Amp CLI: $("$AMP_BIN" version 2>/dev/null || "$AMP_BIN" --version 2>/dev/null || echo unknown)"
echo "Bridge command: $BIN amp"
echo "Amp command inside bridge:"
echo "  $AMP_BIN --no-ide --no-notifications --no-color --dangerously-allow-all --execute --stream-json --stream-json-input --stream-json-thinking --mode $MODEL [threads continue <session>]"
echo

coproc AMP_SERVER {
  AMP_BIN="$AMP_BIN" "$BIN" amp 2>"$tmp_stderr"
}
server_pid="$AMP_SERVER_PID"
server_out="${AMP_SERVER[0]}"
server_in="${AMP_SERVER[1]}"

send_json() {
  local payload="$1"
  echo ">>> $payload"
  printf '%s\n' "$payload" >&"$server_in"
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
  if ! IFS= read -r -t 1 line <&"$server_out"; then
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

eval "exec ${server_in}>&-"
wait "$server_pid"
server_pid=""

if [[ -s "$tmp_stderr" ]]; then
  echo
  echo "Bridge stderr:"
  sed 's/^/!!! /' "$tmp_stderr"
fi

echo
echo "Validating captured stdout against codex_app_server_protocol::JSONRPCMessage / ServerNotification..."
"$BIN" validate-jsonrpc <"$tmp_stdout"

echo
echo "Reconstructed Amp output:"
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
echo "OK: real Amp streamed through the Rust bridge as typed Codex App Server V2 JSON."
