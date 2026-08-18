#!/usr/bin/env bash
# Muesli → Centaur push hook.
#
# Configure inside Muesli (Settings → Meeting Hook → Executable) to point at
# this script. Muesli will fire it after every completed meeting, sending a
# JSON envelope on stdin:
#
#   {"schemaVersion":1,"event":"meeting.completed","kind":"meeting","id":42,
#    "completedAt":"2025-06-10T14:30:00Z"}
#
# We then call `muesli-cli meetings get <id>`, repackage the transcript and
# notes, and POST a single durable workflow run to Centaur. The workflow
# (`muesli_meeting_ingest`) stores the transcript in Postgres and optionally
# fires a Slack notification — see workflows/muesli_meeting_ingest.py.
#
# Required env vars:
#   CENTAUR_API_URL   e.g. https://centaur.example.com
#   MUESLI_API_KEY   an aiv2_* key with scope `workflows:muesli_meeting_ingest`
#
# Optional env vars:
#   MUESLI_CLI            path to muesli-cli (default: /Applications/Muesli.app/Contents/MacOS/muesli-cli)
#   MUESLI_HOST           label for this machine (default: $(hostname -s))
#   MUESLI_PUSH_LOG       log file path (default: ~/Library/Logs/centaur-muesli-push.log)
#   MUESLI_SLACK_CHANNEL  if set, the workflow will also post a summary to
#                         this Slack channel via the in-process slack tool
#
set -euo pipefail

MUESLI_CLI="${MUESLI_CLI:-/Applications/Muesli.app/Contents/MacOS/muesli-cli}"
HOST_LABEL="${MUESLI_HOST:-$(hostname -s)}"
LOG_FILE="${MUESLI_PUSH_LOG:-${HOME}/Library/Logs/centaur-muesli-push.log}"

mkdir -p "$(dirname "$LOG_FILE")"

log() {
    printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" >>"$LOG_FILE"
}

die() {
    log "ERROR $*"
    printf '%s\n' "$*" >&2
    exit 1
}

[ -n "${CENTAUR_API_URL:-}" ] || die "CENTAUR_API_URL is not set"
[ -n "${MUESLI_API_KEY:-}" ] || die "MUESLI_API_KEY is not set"
command -v jq >/dev/null 2>&1 || die "jq is not installed (brew install jq)"
command -v curl >/dev/null 2>&1 || die "curl is not installed"
[ -x "$MUESLI_CLI" ] || die "muesli-cli not found at $MUESLI_CLI (set MUESLI_CLI)"

ENVELOPE="$(cat)"
EVENT="$(printf '%s' "$ENVELOPE" | jq -r '.event // ""')"
KIND="$(printf '%s' "$ENVELOPE" | jq -r '.kind // ""')"
MEETING_ID="$(printf '%s' "$ENVELOPE" | jq -r '.id // empty')"

if [ "$EVENT" != "meeting.completed" ] || [ "$KIND" != "meeting" ]; then
    log "skip event=$EVENT kind=$KIND"
    exit 0
fi

[ -n "$MEETING_ID" ] || die "no meeting id in envelope: $ENVELOPE"

log "fetch meeting_id=$MEETING_ID"
DETAIL="$("$MUESLI_CLI" meetings get "$MEETING_ID")"

PAYLOAD="$(jq -n \
    --argjson detail "$DETAIL" \
    --arg host "$HOST_LABEL" \
    --argjson meeting_id "$MEETING_ID" \
    --arg slack_channel "${MUESLI_SLACK_CHANNEL:-}" \
    '{
        meeting_id: $meeting_id,
        host: $host,
        title: ($detail.title // ""),
        started_at: ($detail.startTime // $detail.started_at // null),
        ended_at: ($detail.endTime // $detail.ended_at // null),
        duration_seconds: ($detail.durationSeconds // $detail.duration_seconds // null),
        word_count: ($detail.wordCount // $detail.word_count // null),
        raw_transcript: ($detail.rawTranscript // $detail.raw_transcript // ""),
        formatted_notes: ($detail.formattedNotes // $detail.formatted_notes // ""),
        notes_state: ($detail.notesState // $detail.notes_state // ""),
        slack_channel: (if $slack_channel == "" then null else $slack_channel end),
        metadata: {
            source: "muesli",
            schema_version: 1,
            calendar_event_id: ($detail.calendarEventId // null),
            folder_id: ($detail.folderId // null),
            template: ($detail.selectedTemplateName // null)
        }
    }')"

BODY="$(jq -n \
    --arg trigger "muesli:${HOST_LABEL}:${MEETING_ID}" \
    --argjson input "$PAYLOAD" \
    '{
        workflow_name: "muesli_meeting_ingest",
        trigger_key: $trigger,
        eager_start: true,
        input: $input
    }')"

log "POST workflow_name=muesli_meeting_ingest trigger=muesli:${HOST_LABEL}:${MEETING_ID}"

HTTP_CODE="$(curl -sS -o /tmp/muesli-push.resp -w '%{http_code}' \
    -X POST "${CENTAUR_API_URL%/}/workflows/runs" \
    -H "Content-Type: application/json" \
    -H "x-api-key: $MUESLI_API_KEY" \
    --data-binary "$BODY")"

RESPONSE="$(cat /tmp/muesli-push.resp || true)"
rm -f /tmp/muesli-push.resp

case "$HTTP_CODE" in
    2*)
        RUN_ID="$(printf '%s' "$RESPONSE" | jq -r '.run_id // .id // empty')"
        log "ok http=$HTTP_CODE run_id=$RUN_ID"
        printf '%s\n' "$RESPONSE"
        ;;
    *)
        log "fail http=$HTTP_CODE body=$RESPONSE"
        die "Centaur rejected workflow run (HTTP $HTTP_CODE): $RESPONSE"
        ;;
esac
