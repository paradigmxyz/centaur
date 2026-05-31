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

PROMPT="${1:-Reply with exactly: CENTAUR_HARNESS_COMPARE_OK}"
TIMEOUT_S="${TIMEOUT_S:-240}"
READ_TIMEOUT_S="${READ_TIMEOUT_S:-5}"
HARNESSES="${HARNESSES:-codex claude-code amp}"
PARALLEL="${PARALLEL:-1}"
LOG_DIR="${LOG_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/harness-server-compare.XXXXXX")}"
COMBINED_LOG="$LOG_DIR/combined.log"

mkdir -p "$LOG_DIR"
: >"$COMBINED_LOG"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required" >&2
  exit 2
fi

if [[ "${BUILD:-auto}" == "1" || ! -x "$BIN" ]]; then
  echo "Building harness-server because the binary is missing or BUILD=1 was set."
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
  local name="$1"
  local stream="$2"
  local line="$3"
  printf '%s [%s] [%s] %s\n' "$(ts)" "$name" "$stream" "$line" | tee -a "$COMBINED_LOG"
}

json_for_thread_start() {
  local name="$1"
  case "$name" in
    codex)
      local model="${CODEX_SMOKE_MODEL:-${CODEX_MODEL:-}}"
      if [[ -n "$model" ]]; then
        jq -nc --arg model "$model" \
          '{id:2, method:"thread/start", params:{model:$model, approvalPolicy:"never", sandbox:"danger-full-access"}}'
      else
        jq -nc '{id:2, method:"thread/start", params:{approvalPolicy:"never", sandbox:"danger-full-access"}}'
      fi
      ;;
    claude-code)
      jq -nc --arg model "${CENTAUR_REAL_CLAUDE_MODEL:-${CLAUDE_MODEL:-sonnet}}" \
        '{id:2, method:"thread/start", params:{model:$model}}'
      ;;
    amp)
      jq -nc --arg model "${AMP_MODE:-deep}" \
        '{id:2, method:"thread/start", params:{model:$model}}'
      ;;
    *)
      echo "unknown harness: $name" >&2
      return 2
      ;;
  esac
}

server_env_and_command() {
  local name="$1"
  case "$name" in
    codex)
      if [[ -n "${CODEX_BIN:-}" ]]; then
        CODEX_BIN="$CODEX_BIN" exec "$BIN" codex
      else
        exec "$BIN" codex
      fi
      ;;
    claude-code)
      CLAUDE_BIN="${CLAUDE_BIN:-claude}" exec "$BIN" claude-code
      ;;
    amp)
      AMP_BIN="${AMP_BIN:-amp}" exec "$BIN" amp
      ;;
    *)
      echo "unknown harness: $name" >&2
      exit 2
      ;;
  esac
}

run_one() (
  local name="$1"
  local stdout_log="$LOG_DIR/$name.stdout.ndjson"
  local stderr_log="$LOG_DIR/$name.stderr.log"
  local assistant_text_file="$LOG_DIR/$name.assistant.txt"
  local summary="$LOG_DIR/$name.summary"
  : >"$stdout_log"
  : >"$stderr_log"
  : >"$assistant_text_file"
  : >"$summary"

  local server_pid=""
  cleanup_one() {
    if [[ -n "$server_pid" ]] && kill -0 "$server_pid" >/dev/null 2>&1; then
      kill "$server_pid" >/dev/null 2>&1 || true
      wait "$server_pid" >/dev/null 2>&1 || true
    fi
  }
  trap cleanup_one EXIT

  log "$name" "meta" "starting: $BIN $name"
  case "$name" in
    codex) log "$name" "meta" "inner command: ${CODEX_BIN:-codex-auto} app-server --listen stdio://" ;;
    claude-code)
      log "$name" "meta" "inner command: ${CLAUDE_BIN:-claude} --print --input-format stream-json --output-format stream-json --verbose --include-partial-messages --dangerously-skip-permissions --permission-mode bypassPermissions --model ${CENTAUR_REAL_CLAUDE_MODEL:-${CLAUDE_MODEL:-sonnet}} --session-id <thread-id>"
      ;;
    amp)
      log "$name" "meta" "inner command: ${AMP_BIN:-amp} --no-ide --no-notifications --no-color --dangerously-allow-all --execute --stream-json --stream-json-input --stream-json-thinking --mode ${AMP_MODE:-deep}"
      ;;
  esac

  coproc SERVER { server_env_and_command "$name" 2>"$stderr_log"; }
  server_pid="$SERVER_PID"
  local server_out="${SERVER[0]}"
  local server_in="${SERVER[1]}"

  send_json() {
    local payload="$1"
    log "$name" "stdin" "$payload"
    printf '%s\n' "$payload" >&"$server_in"
  }

  send_json "$(jq -nc '{id:1, method:"initialize", params:{clientInfo:{name:"compare-real-harnesses", title:null, version:"0"}, capabilities:null}}')"
  send_json "$(json_for_thread_start "$name")"

  local assistant_text=""
  local thread_id=""
  local turn_status=""
  local turn_error=""
  local sent_turn_start=0
  local agent_delta_count=0
  local reasoning_delta_count=0
  local non_json_count=0
  local deadline=$((SECONDS + TIMEOUT_S))

  while (( SECONDS < deadline )); do
    local line=""
    if ! IFS= read -r -t "$READ_TIMEOUT_S" line <&"$server_out"; then
      if ! kill -0 "$server_pid" >/dev/null 2>&1; then
        break
      fi
      continue
    fi

    log "$name" "stdout" "$line"
    printf '%s\n' "$line" >>"$stdout_log"

    if ! jq -e . >/dev/null 2>&1 <<<"$line"; then
      non_json_count=$((non_json_count + 1))
      continue
    fi

    local id method
    id="$(jq -r '.id // empty' <<<"$line")"
    method="$(jq -r '.method // empty' <<<"$line")"

    if [[ "$id" == "2" && "$sent_turn_start" == "0" ]]; then
      thread_id="$(jq -r '.result.thread.id // empty' <<<"$line")"
      if [[ -z "$thread_id" ]]; then
        turn_status="failed"
        turn_error="thread/start did not return result.thread.id"
        break
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
      local delta
      delta="$(jq -r '.params.delta // ""' <<<"$line")"
      assistant_text="${assistant_text}${delta}"
      jq -j '.params.delta // ""' <<<"$line" >>"$assistant_text_file"
      agent_delta_count=$((agent_delta_count + 1))
      log "$name" "delta" "agent #$agent_delta_count $(jq -r '.params.delta // ""' <<<"$line" | perl -0pe 's/\n/\\n/g')"
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

  if (( SECONDS >= deadline )) && [[ -z "$turn_status" ]]; then
    turn_status="timeout"
    turn_error="timed out after ${TIMEOUT_S}s"
  fi

  eval "exec ${server_in}>&-" || true
  wait "$server_pid" >/dev/null 2>&1 || true
  server_pid=""
  trap - EXIT

  if [[ -s "$stderr_log" ]]; then
    while IFS= read -r line; do
      log "$name" "stderr" "$line"
    done <"$stderr_log"
  fi

  local validation="not-run"
  if "$BIN" validate-jsonrpc <"$stdout_log" >>"$LOG_DIR/$name.validation.log" 2>&1; then
    validation="ok"
    log "$name" "validation" "ok"
  else
    validation="failed"
    while IFS= read -r line; do
      log "$name" "validation" "$line"
    done <"$LOG_DIR/$name.validation.log"
  fi

  local delta_validation="not-run"
  if "$BIN" validate-agent-deltas <"$stdout_log" >>"$LOG_DIR/$name.delta-validation.log" 2>&1; then
    delta_validation="ok"
    log "$name" "delta-validation" "ok"
  else
    delta_validation="failed"
    while IFS= read -r line; do
      log "$name" "delta-validation" "$line"
    done <"$LOG_DIR/$name.delta-validation.log"
  fi

  local assistant_text_summary
  assistant_text_summary="$(perl -0pe 's/\\/\\\\/g; s/\n/\\n/g' "$assistant_text_file")"

  {
    printf 'name=%s\n' "$name"
    printf 'status=%s\n' "${turn_status:-missing}"
    printf 'error=%s\n' "$turn_error"
    printf 'agent_delta_count=%s\n' "$agent_delta_count"
    printf 'reasoning_delta_count=%s\n' "$reasoning_delta_count"
    printf 'non_json_stdout=%s\n' "$non_json_count"
    printf 'stderr_bytes=%s\n' "$(wc -c <"$stderr_log" | tr -d ' ')"
    printf 'validation=%s\n' "$validation"
    printf 'delta_validation=%s\n' "$delta_validation"
    printf 'stdout_log=%s\n' "$stdout_log"
    printf 'stderr_log=%s\n' "$stderr_log"
    printf 'assistant_text_log=%s\n' "$assistant_text_file"
    printf 'assistant_text=%s\n' "$assistant_text_summary"
  } >"$summary"

  log "$name" "summary" "status=${turn_status:-missing} deltas=$agent_delta_count validation=$validation delta_validation=$delta_validation text=$assistant_text_summary"

  [[ "${turn_status:-missing}" == "completed" ]] &&
    [[ "$validation" == "ok" ]] &&
    [[ "$delta_validation" == "ok" ]] &&
    [[ "$non_json_count" == "0" ]]
)

log "compare" "meta" "log dir: $LOG_DIR"
log "compare" "meta" "same user input: $PROMPT"
log "compare" "meta" "harnesses: $HARNESSES"
log "compare" "meta" "parallel: $PARALLEL"

validate_harness_name() {
  case "$1" in
    codex | claude-code | amp) ;;
    *)
      echo "unknown harness in HARNESSES: $1" >&2
      exit 2
      ;;
  esac
}

failed=0
if [[ "$PARALLEL" == "0" ]]; then
  for harness in $HARNESSES; do
    validate_harness_name "$harness"
    if ! run_one "$harness"; then
      failed=1
    fi
  done
else
  pids=()
  for harness in $HARNESSES; do
    validate_harness_name "$harness"
    run_one "$harness" &
    pids+=("$!")
  done

  for pid in "${pids[@]}"; do
    if ! wait "$pid"; then
      failed=1
    fi
  done
fi

echo
echo "Log dir: $LOG_DIR"
echo "Combined log: $COMBINED_LOG"
echo
echo "Summary:"
for summary in "$LOG_DIR"/*.summary; do
  echo "--- $(basename "$summary")"
  cat "$summary"
done | tee -a "$COMBINED_LOG"

exit "$failed"
