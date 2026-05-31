#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPARE="$ROOT/scripts/compare-real-harnesses.sh"
BIN="$ROOT/target/debug/harness-server"

TIMEOUT_S="${TIMEOUT_S:-300}"
LOG_DIR="${LOG_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/harness-server-stress.XXXXXX")}"
HARNESSES="${HARNESSES:-codex claude-code}"
CASES="${CASES:-long_stream multi_command stderr_stdout failing_command mixed_format large_output quoted_json_output temp_file_roundtrip fail_then_success tabs_quotes}"
SUMMARY="$LOG_DIR/summary.tsv"

mkdir -p "$LOG_DIR"
: >"$SUMMARY"
printf 'case\tharness\tstatus\tvalidation\tnon_json_stdout\tagent_deltas\treasoning_deltas\tcommand_completed\tcommand_failed\tassistant_text\n' >"$SUMMARY"

if [[ "${BUILD:-auto}" == "1" || ! -x "$BIN" ]]; then
  cargo build --manifest-path "$ROOT/Cargo.toml" --bin harness-server >/dev/null
fi

prompt_for_case() {
  case "$1" in
    long_stream)
      cat <<'PROMPT'
Reply without using tools. Produce exactly eight numbered lines. Each line must start with LINE-01 through LINE-08, include the phrase stream-check, and the final line must end with LONG_STREAM_DONE.
PROMPT
      ;;
    multi_command)
      cat <<'PROMPT'
Use the shell command tool twice, and do not skip either call.
First run: printf FIRST_OK
Second run: printf SECOND_OK
After both commands succeed, reply exactly: MULTI_COMMAND_DONE
PROMPT
      ;;
    stderr_stdout)
      cat <<'PROMPT'
Use the shell command tool once. Run exactly:
sh -lc 'printf STDOUT_OK; printf STDERR_OK >&2'
After the command succeeds, reply exactly: STDERR_STDOUT_DONE
PROMPT
      ;;
    failing_command)
      cat <<'PROMPT'
Use the shell command tool once. Run exactly:
sh -lc 'printf FAIL_STDOUT; printf FAIL_STDERR >&2; exit 7'
The command is supposed to fail. After observing the failure, reply exactly: FAILING_COMMAND_DONE
PROMPT
      ;;
    mixed_format)
      cat <<'PROMPT'
Use the shell command tool once to run:
printf 'alpha\nbeta\ngamma\n'
Then reply with exactly this JSON on one line and no markdown:
{"status":"ok","rows":["alpha","beta","gamma"],"done":"MIXED_FORMAT_DONE"}
PROMPT
      ;;
    large_output)
      cat <<'PROMPT'
Use the shell command tool once. Run exactly:
sh -lc 'for i in $(seq -w 1 40); do printf "ROW-%s stream-payload-%s\n" "$i" "$i"; done'
After reading all output, reply exactly: LARGE_OUTPUT_DONE
PROMPT
      ;;
    quoted_json_output)
      cat <<'PROMPT'
Use the shell command tool once. Run exactly:
sh -lc 'printf "{\"alpha\":\"a b\",\"quote\":\"\\\"\",\"slash\":\"\\\\\",\"list\":[1,2,3]}\n"'
After the command succeeds, reply exactly: QUOTED_JSON_OUTPUT_DONE
PROMPT
      ;;
    temp_file_roundtrip)
      cat <<'PROMPT'
Use the shell command tool once. Run exactly:
sh -lc 'tmp=$(mktemp); printf "file-line-1\nfile-line-2\n" > "$tmp"; cat "$tmp"; rm "$tmp"'
After the command succeeds, reply exactly: TEMP_FILE_ROUNDTRIP_DONE
PROMPT
      ;;
    fail_then_success)
      cat <<'PROMPT'
Use the shell command tool twice, and do not skip either call.
First run exactly: sh -lc 'printf FIRST_FAIL_STDOUT; printf FIRST_FAIL_STDERR >&2; exit 3'
Second run exactly: printf SECOND_SUCCESS
The first command is supposed to fail and the second is supposed to succeed. After both have run, reply exactly: FAIL_THEN_SUCCESS_DONE
PROMPT
      ;;
    tabs_quotes)
      cat <<'PROMPT'
Use the shell command tool once. Run exactly:
sh -lc 'printf "tab\tvalue\nquote:\" slash:\\\\ end\n"'
Then reply with exactly this JSON on one line and no markdown:
{"status":"ok","done":"TABS_QUOTES_DONE","saw":["tab","quote","slash"]}
PROMPT
      ;;
    *)
      echo "unknown stress case: $1" >&2
      return 2
      ;;
  esac
}

summarize_case() {
  local case_name="$1"
  local case_dir="$2"
  for harness in $HARNESSES; do
    local summary_file="$case_dir/$harness.summary"
    local stdout_file="$case_dir/$harness.stdout.ndjson"
    if [[ ! -s "$summary_file" || ! -s "$stdout_file" ]]; then
      printf '%s\t%s\tmissing\tmissing\tmissing\t0\t0\t0\t0\t\n' "$case_name" "$harness" >>"$SUMMARY"
      continue
    fi
    local status validation non_json agent_deltas reasoning_deltas assistant_text
    status="$(awk -F= '$1=="status"{print $2}' "$summary_file")"
    validation="$(awk -F= '$1=="validation"{print $2}' "$summary_file")"
    non_json="$(awk -F= '$1=="non_json_stdout"{print $2}' "$summary_file")"
    agent_deltas="$(awk -F= '$1=="agent_delta_count"{print $2}' "$summary_file")"
    reasoning_deltas="$(awk -F= '$1=="reasoning_delta_count"{print $2}' "$summary_file")"
    assistant_text="$(awk -F= '$1=="assistant_text"{print substr($0, index($0,$2))}' "$summary_file" | tr '\n' ' ')"
    local command_completed command_failed
    command_completed="$(jq -R -s -r '[split("\n")[] | select(length > 0) | (try fromjson catch empty) | select(.method == "item/completed" and .params.item.type == "commandExecution" and .params.item.status == "completed")] | length' "$stdout_file")"
    command_failed="$(jq -R -s -r '[split("\n")[] | select(length > 0) | (try fromjson catch empty) | select(.method == "item/completed" and .params.item.type == "commandExecution" and .params.item.status == "failed")] | length' "$stdout_file")"
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$case_name" "$harness" "$status" "$validation" "$non_json" "$agent_deltas" \
      "$reasoning_deltas" "$command_completed" "$command_failed" "$assistant_text" >>"$SUMMARY"
  done
}

echo "Stress log dir: $LOG_DIR"
echo "Cases: $CASES"
echo "Harnesses: $HARNESSES"

for case_name in $CASES; do
  case_dir="$LOG_DIR/$case_name"
  mkdir -p "$case_dir"
  prompt="$(prompt_for_case "$case_name")"
  printf '%s\n' "$prompt" >"$case_dir/prompt.txt"
  echo
  echo "=== $case_name"
  HARNESSES="$HARNESSES" PARALLEL=0 LOG_DIR="$case_dir" BUILD=0 TIMEOUT_S="$TIMEOUT_S" \
    "$COMPARE" "$prompt"
  summarize_case "$case_name" "$case_dir"
done

echo
echo "Stress summary:"
column -t -s $'\t' "$SUMMARY" 2>/dev/null || cat "$SUMMARY"
echo
echo "Log dir: $LOG_DIR"
