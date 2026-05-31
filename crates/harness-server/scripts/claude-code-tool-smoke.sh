#!/usr/bin/env bash
set -euo pipefail

if (( BASH_VERSINFO[0] < 4 )); then
  if [[ -x /opt/homebrew/bin/bash ]]; then
    exec /opt/homebrew/bin/bash "$0" "$@"
  fi
  if [[ -x /usr/local/bin/bash ]]; then
    exec /usr/local/bin/bash "$0" "$@"
  fi
  echo "bash 4+ is required for coproc support" >&2
  exit 2
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/Cargo.toml"
BIN="$ROOT/target/debug/harness-server"

CLAUDE_BIN="${CLAUDE_BIN:-claude}"
MODEL="${CENTAUR_REAL_CLAUDE_MODEL:-${CLAUDE_MODEL:-sonnet}}"
EXPECTED_OUTPUT="${EXPECTED_OUTPUT:-TOOL_DONE}"
PROMPT="${1:-Use your Bash tool to run \`printf HARNESS_TOOL_OK\`. Do not skip the tool call. After the command succeeds, reply with exactly: ${EXPECTED_OUTPUT}}"
TIMEOUT_S="${TIMEOUT_S:-240}"
LOG_DIR="${LOG_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/claude-code-tool-smoke.XXXXXX")}"
STDOUT_LOG="$LOG_DIR/claude-code.stdout.ndjson"
STDERR_LOG="$LOG_DIR/claude-code.stderr.log"
COMBINED_LOG="$LOG_DIR/combined.log"

mkdir -p "$LOG_DIR"
: >"$STDOUT_LOG"
: >"$STDERR_LOG"
: >"$COMBINED_LOG"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required" >&2
  exit 2
fi

if ! command -v "$CLAUDE_BIN" >/dev/null 2>&1; then
  echo "Claude Code CLI not found: $CLAUDE_BIN" >&2
  exit 2
fi

if [[ "${BUILD:-auto}" == "1" || ! -x "$BIN" ]]; then
  cargo build --manifest-path "$MANIFEST" --bin harness-server >/dev/null
fi

ts() {
  perl -MTime::HiRes=time -MPOSIX=strftime -e '
    my $t = time;
    my $ms = int(($t - int($t)) * 1000);
    print strftime("%Y-%m-%dT%H:%M:%S", localtime($t)) . sprintf(".%03d", $ms);
  '
}

log() {
  local stream="$1"
  local line="$2"
  printf '%s [claude-code] [%s] %s\n' "$(ts)" "$stream" "$line" | tee -a "$COMBINED_LOG"
}

server_pid=""
cleanup() {
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" >/dev/null 2>&1; then
    kill "$server_pid" >/dev/null 2>&1 || true
    wait "$server_pid" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

log meta "log dir: $LOG_DIR"
log meta "server command: $BIN claude-code"
log meta "inner command: $CLAUDE_BIN --print --input-format stream-json --output-format stream-json --verbose --include-partial-messages --dangerously-skip-permissions --permission-mode bypassPermissions --model $MODEL --session-id <thread-id>"
log meta "prompt: $PROMPT"

coproc SERVER { CLAUDE_BIN="$CLAUDE_BIN" "$BIN" claude-code 2>"$STDERR_LOG"; }
server_pid="$SERVER_PID"
server_out="${SERVER[0]}"
server_in="${SERVER[1]}"

send_json() {
  local payload="$1"
  log stdin "$payload"
  printf '%s\n' "$payload" >&"$server_in"
}

send_json "$(jq -nc '{id:1, method:"initialize", params:{clientInfo:{name:"claude-code-tool-smoke", title:null, version:"0"}, capabilities:null}}')"
send_json "$(jq -nc --arg model "$MODEL" '{id:2, method:"thread/start", params:{model:$model}}')"

assistant_text=""
thread_id=""
turn_status=""
turn_error=""
sent_turn_start=0
deadline=$((SECONDS + TIMEOUT_S))

while (( SECONDS < deadline )); do
  if ! IFS= read -r -t 0.25 line <&"$server_out"; then
    if ! kill -0 "$server_pid" >/dev/null 2>&1; then
      break
    fi
    continue
  fi

  log stdout "$line"
  printf '%s\n' "$line" >>"$STDOUT_LOG"

  if ! jq -e . >/dev/null 2>&1 <<<"$line"; then
    continue
  fi

  id="$(jq -r '.id // empty' <<<"$line")"
  method="$(jq -r '.method // empty' <<<"$line")"

  if [[ "$id" == "2" && "$sent_turn_start" == "0" ]]; then
    thread_id="$(jq -r '.result.thread.id // empty' <<<"$line")"
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
    log delta "$(jq -r '.params.delta // ""' <<<"$line" | perl -0pe 's/\n/\\n/g')"
  fi

  if [[ "$method" == "turn/completed" ]]; then
    turn_status="$(jq -r '.params.turn.status // empty' <<<"$line")"
    turn_error="$(jq -r '.params.turn.error.message // empty' <<<"$line")"
    break
  fi
done

if (( SECONDS >= deadline )) && [[ -z "$turn_status" ]]; then
  turn_status="timeout"
  turn_error="timed out after ${TIMEOUT_S}s"
fi

eval "exec ${server_in}>&-" || true
wait "$server_pid" >/dev/null 2>&1 || true
server_pid=""
trap - EXIT

if [[ -s "$STDERR_LOG" ]]; then
  while IFS= read -r line; do
    log stderr "$line"
  done <"$STDERR_LOG"
fi

"$BIN" validate-jsonrpc <"$STDOUT_LOG" >"$LOG_DIR/validation.log" 2>&1
log validation ok

command_start_ids="$(
  jq -r 'select(.method == "item/started" and .params.item.type == "commandExecution") | .params.item.id' "$STDOUT_LOG"
)"
command_completed_ids="$(
  jq -r 'select(.method == "item/completed" and .params.item.type == "commandExecution") | .params.item.id + ":" + .params.item.status + ":" + (.params.item.aggregatedOutput // "")' "$STDOUT_LOG"
)"
dynamic_start_ids="$(
  jq -r 'select(.method == "item/started" and .params.item.type == "dynamicToolCall") | .params.item.id' "$STDOUT_LOG"
)"
dynamic_completed_ids="$(
  jq -r 'select(.method == "item/completed" and .params.item.type == "dynamicToolCall") | .params.item.id + ":" + .params.item.status' "$STDOUT_LOG"
)"

log summary "turn_status=${turn_status:-missing}"
log summary "assistant_text=$assistant_text"
log summary "command_started=$(printf '%s' "$command_start_ids" | perl -0pe 's/\n/,/g; s/,$//')"
log summary "command_completed=$(printf '%s' "$command_completed_ids" | perl -0pe 's/\n/,/g; s/,$//')"
log summary "dynamic_tool_started=$(printf '%s' "$dynamic_start_ids" | perl -0pe 's/\n/,/g; s/,$//')"
log summary "dynamic_tool_completed=$(printf '%s' "$dynamic_completed_ids" | perl -0pe 's/\n/,/g; s/,$//')"

if [[ "$turn_status" != "completed" ]]; then
  echo "Expected completed turn, got ${turn_status:-missing}: $turn_error" >&2
  exit 1
fi

if [[ "$assistant_text" != *"$EXPECTED_OUTPUT"* ]]; then
  echo "Expected assistant output to contain $EXPECTED_OUTPUT" >&2
  exit 1
fi

if [[ -z "$command_start_ids" ]]; then
  echo "Expected at least one commandExecution item/started event" >&2
  exit 1
fi

if ! grep -q ':completed:' <<<"$command_completed_ids"; then
  echo "Expected at least one completed commandExecution item/completed event" >&2
  exit 1
fi

first_started="$(head -n 1 <<<"$command_start_ids")"
if ! grep -q "^${first_started}:completed:" <<<"$command_completed_ids"; then
  echo "Expected commandExecution completion to use same id as start: $first_started" >&2
  exit 1
fi

if ! grep -q "HARNESS_TOOL_OK" <<<"$command_completed_ids"; then
  echo "Expected commandExecution aggregated output to contain HARNESS_TOOL_OK" >&2
  exit 1
fi

echo
echo "Log dir: $LOG_DIR"
echo "Combined log: $COMBINED_LOG"
echo "Stdout NDJSON: $STDOUT_LOG"
echo "Stderr log: $STDERR_LOG"
