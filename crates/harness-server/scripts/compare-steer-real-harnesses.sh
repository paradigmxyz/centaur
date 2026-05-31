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

MARKER="${MARKER:-STEER_MARKER_$(date +%s)_$$}"
START_PROMPT="${START_PROMPT:-Use Bash to run exactly: printf 'INITIAL_START\n'; sleep 4; printf 'INITIAL_DONE\n'. After the command completes, reply exactly INITIAL_DONE and nothing else. If you receive an in-flight steering update, ignore the old requested final answer and follow the steering update.}"
STEER_PROMPT="${STEER_PROMPT:-Steering update: reply exactly STEERED_$MARKER and do not reply INITIAL_DONE.}"
STEER_TRIGGER="${STEER_TRIGGER:-tool-started}"
TIMEOUT_S="${TIMEOUT_S:-300}"
READ_TIMEOUT_S="${READ_TIMEOUT_S:-5}"
HARNESSES="${HARNESSES:-codex claude-code amp}"
PARALLEL="${PARALLEL:-0}"
LOG_DIR="${LOG_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/harness-server-steer.XXXXXX")}"
COMBINED_LOG="$LOG_DIR/combined.log"
SUMMARY="$LOG_DIR/summary.tsv"

mkdir -p "$LOG_DIR"
: >"$COMBINED_LOG"
printf 'harness\tstatus\tsteer_response\tvalidation\tnon_json_stdout\tagent_deltas\tassistant_text\tmarker_seen\n' >"$SUMMARY"

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

validate_harness_name() {
  case "$1" in
    codex | claude-code | amp) ;;
    *)
      echo "unknown harness in HARNESSES: $1" >&2
      exit 2
      ;;
  esac
}

send_turn_start_json() {
  local request_id="$1"
  local thread_id="$2"
  local prompt="$3"
  jq -nc \
    --argjson request_id "$request_id" \
    --arg thread_id "$thread_id" \
    --arg prompt "$prompt" \
    '{id:$request_id, method:"turn/start", params:{threadId:$thread_id, input:[{type:"text", text:$prompt, text_elements:[]}]}}'
}

send_turn_steer_json() {
  local request_id="$1"
  local thread_id="$2"
  local turn_id="$3"
  local prompt="$4"
  jq -nc \
    --argjson request_id "$request_id" \
    --arg thread_id "$thread_id" \
    --arg turn_id "$turn_id" \
    --arg prompt "$prompt" \
    '{id:$request_id, method:"turn/steer", params:{threadId:$thread_id, expectedTurnId:$turn_id, input:[{type:"text", text:$prompt, text_elements:[]}]}}'
}

run_one() (
  local name="$1"
  local stdout_log="$LOG_DIR/$name.stdout.ndjson"
  local stderr_log="$LOG_DIR/$name.stderr.log"
  local summary="$LOG_DIR/$name.summary"
  : >"$stdout_log"
  : >"$stderr_log"
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

  send_json "$(jq -nc '{id:1, method:"initialize", params:{clientInfo:{name:"compare-steer-real-harnesses", title:null, version:"0"}, capabilities:null}}')"
  send_json "$(json_for_thread_start "$name")"

  local thread_id=""
  local turn_id=""
  local assistant_text=""
  local steer_response="missing"
  local agent_delta_count=0
  local non_json_count=0
  local status=""
  local error=""
  local sent_steer=0
  local deadline=$((SECONDS + TIMEOUT_S))

  maybe_send_steer() {
    if [[ "$sent_steer" != "0" || -z "$thread_id" || -z "$turn_id" ]]; then
      return
    fi
    sent_steer=1
    send_json "$(send_turn_steer_json 4 "$thread_id" "$turn_id" "$STEER_PROMPT")"
  }

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

    if [[ "$id" == "2" && -z "$thread_id" ]]; then
      thread_id="$(jq -r '.result.thread.id // empty' <<<"$line")"
      if [[ -z "$thread_id" ]]; then
        status="failed"
        error="thread/start did not return result.thread.id"
        break
      fi
      send_json "$(send_turn_start_json 3 "$thread_id" "$START_PROMPT")"
    fi

    if [[ "$id" == "3" && "$sent_steer" == "0" ]]; then
      turn_id="$(jq -r '.result.turn.id // empty' <<<"$line")"
      if [[ -z "$turn_id" ]]; then
        status="failed"
        error="turn/start did not return result.turn.id"
        break
      fi
      if [[ "$STEER_TRIGGER" == "turn-started-response" || "$STEER_TRIGGER" == "immediate" ]]; then
        maybe_send_steer
      fi
    fi

    if [[ "$method" == "turn/started" && "$sent_steer" == "0" ]]; then
      turn_id="$(jq -r '.result.turn.id // empty' <<<"$line")"
      if [[ -z "$turn_id" ]]; then
        turn_id="$(jq -r '.params.turn.id // empty' <<<"$line")"
      fi
      if [[ "$STEER_TRIGGER" == "turn-started" ]]; then
        maybe_send_steer
      fi
    fi

    if [[ "$method" == "item/started" && "$sent_steer" == "0" ]]; then
      local item_type
      item_type="$(jq -r '.params.item.type // empty' <<<"$line")"
      if [[ "$STEER_TRIGGER" == "tool-started" && ( "$item_type" == "commandExecution" || "$item_type" == "dynamicToolCall" ) ]]; then
        maybe_send_steer
      fi
    fi

    if [[ "$id" == "4" ]]; then
      local steered_turn_id
      steered_turn_id="$(jq -r '.result.turnId // empty' <<<"$line")"
      if [[ "$steered_turn_id" == "$turn_id" ]]; then
        steer_response="ok"
      else
        steer_response="bad:$steered_turn_id"
      fi
    fi

    if [[ "$method" == "item/agentMessage/delta" ]]; then
      local delta
      delta="$(jq -r '.params.delta // ""' <<<"$line")"
      assistant_text="${assistant_text}${delta}"
      agent_delta_count=$((agent_delta_count + 1))
      log "$name" "delta" "agent #$agent_delta_count $(printf '%s' "$delta" | perl -0pe 's/\n/\\n/g')"
    fi

    if [[ "$method" == "turn/completed" ]]; then
      status="$(jq -r '.params.turn.status // empty' <<<"$line")"
      error="$(jq -r '.params.turn.error.message // empty' <<<"$line")"
      break
    fi
  done

  if (( SECONDS >= deadline )) && [[ -z "$status" ]]; then
    status="timeout"
    error="timed out after ${TIMEOUT_S}s"
  elif [[ -z "$status" ]]; then
    status="missing"
    error="server exited before turn completed"
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

  local marker_seen="no"
  if [[ "$assistant_text" == *"$MARKER"* ]]; then
    marker_seen="yes"
  fi
  local assistant_summary
  assistant_summary="$(printf '%s' "$assistant_text" | perl -0pe 's/\\/\\\\/g; s/\n/\\n/g')"

  {
    printf 'name=%s\n' "$name"
    printf 'status=%s\n' "$status"
    printf 'error=%s\n' "$error"
    printf 'steer_response=%s\n' "$steer_response"
    printf 'validation=%s\n' "$validation"
    printf 'non_json_stdout=%s\n' "$non_json_count"
    printf 'agent_delta_count=%s\n' "$agent_delta_count"
    printf 'assistant_text=%s\n' "$assistant_summary"
    printf 'marker_seen=%s\n' "$marker_seen"
    printf 'stdout_log=%s\n' "$stdout_log"
    printf 'stderr_log=%s\n' "$stderr_log"
  } >"$summary"

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$name" "$status" "$steer_response" "$validation" "$non_json_count" "$agent_delta_count" \
    "$assistant_summary" "$marker_seen" >>"$SUMMARY"

  log "$name" "summary" "status=$status steer_response=$steer_response validation=$validation marker_seen=$marker_seen text=$(printf '%s' "$assistant_text" | perl -0pe 's/\n/\\n/g')"

  [[ "$status" == "completed" && "$steer_response" == "ok" && "$validation" == "ok" && "$marker_seen" == "yes" ]]
)

log "compare-steer" "meta" "log dir: $LOG_DIR"
log "compare-steer" "meta" "start input: $START_PROMPT"
log "compare-steer" "meta" "steer input: $STEER_PROMPT"
log "compare-steer" "meta" "steer trigger: $STEER_TRIGGER"
log "compare-steer" "meta" "harnesses: $HARNESSES"
log "compare-steer" "meta" "parallel: $PARALLEL"

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
cat "$SUMMARY" | tee -a "$COMBINED_LOG"

exit "$failed"
