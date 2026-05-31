#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BRIDGE_MANIFEST="$ROOT/Cargo.toml"
BRIDGE_BIN="$ROOT/target/debug/harness-server"

CODEX_BIN="${CODEX_BIN:-codex}"
MODEL="${CODEX_SMOKE_MODEL:-${CODEX_MODEL:-}}"
EXPECTED_OUTPUT="${EXPECTED_OUTPUT:-CENTAUR_CODEX_APP_SERVER_OK}"
PROMPT="${1:-Reply with exactly: ${EXPECTED_OUTPUT}}"
TIMEOUT_S="${TIMEOUT_S:-180}"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required for this smoke test" >&2
  exit 2
fi

if ! command -v "$CODEX_BIN" >/dev/null 2>&1; then
  echo "Codex CLI not found: $CODEX_BIN" >&2
  echo "Set CODEX_BIN=/path/to/codex if it is not on PATH." >&2
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

echo "Building typed JSON-RPC validator..."
cargo build --manifest-path "$BRIDGE_MANIFEST" --bin harness-server >/dev/null

echo "Codex CLI: $("$CODEX_BIN" --version)"
echo "Bridge command: $BRIDGE_BIN codex"
echo "Codex command inside bridge: $CODEX_BIN app-server --listen stdio://"
if [[ -n "$MODEL" ]]; then
  echo "Requested model: $MODEL"
else
  echo "Requested model: <Codex default>"
fi
echo

coproc CODEX_SERVER {
  CODEX_BIN="$CODEX_BIN" "$BRIDGE_BIN" codex 2>"$tmp_stderr"
}
server_pid="$CODEX_SERVER_PID"
server_out="${CODEX_SERVER[0]}"
server_in="${CODEX_SERVER[1]}"

send_json() {
  local payload="$1"
  echo ">>> $payload"
  printf '%s\n' "$payload" >&"$server_in"
}

now_ms() {
  perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000'
}

send_json "$(jq -nc '{id:1, method:"initialize", params:{clientInfo:{name:"bash-smoke", title:null, version:"0"}, capabilities:null}}')"
if [[ -n "$MODEL" ]]; then
  send_json "$(jq -nc --arg model "$MODEL" '{id:2, method:"thread/start", params:{model:$model, approvalPolicy:"never", sandbox:"danger-full-access"}}')"
else
  send_json "$(jq -nc '{id:2, method:"thread/start", params:{approvalPolicy:"never", sandbox:"danger-full-access"}}')"
fi

assistant_text=""
thread_id=""
turn_status=""
turn_error=""
sent_turn_start=0
agent_delta_count=0
reasoning_delta_count=0
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
    echo "Codex app-server emitted non-JSON stdout line" >&2
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
        '{id:3, method:"turn/start", params:{threadId:$thread_id, input:[{type:"text", text:$prompt, text_elements:[]}]}}'
    )"
    sent_turn_start=1
  fi

  if [[ "$method" == "item/agentMessage/delta" ]]; then
    delta="$(jq -r '.params.delta // ""' <<<"$line")"
    assistant_text="${assistant_text}${delta}"
    agent_delta_count=$((agent_delta_count + 1))
    echo "--- agent delta #$agent_delta_count: $(jq -r '.params.delta // ""' <<<"$line" | perl -0pe 's/\n/\\n/g')"
  fi

  if [[ "$method" == item/reasoning/* ]]; then
    reasoning_delta_count=$((reasoning_delta_count + 1))
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
  echo "Codex app-server stderr:"
  sed 's/^/!!! /' "$tmp_stderr"
fi

echo
echo "Validating captured stdout against codex_app_server_protocol::JSONRPCMessage / ServerNotification..."
"$BRIDGE_BIN" validate-jsonrpc <"$tmp_stdout"

echo
echo "Reconstructed Codex output:"
printf '%s\n' "$assistant_text"
echo
echo "Agent message delta events: $agent_delta_count"
echo "Reasoning delta events: $reasoning_delta_count"

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
echo "OK: upstream Codex app-server streamed typed Codex App Server V2 JSON."
