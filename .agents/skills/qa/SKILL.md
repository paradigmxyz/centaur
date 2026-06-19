---
name: qa
description: "Smoke test the full running Centaur system from a new Slack thread. Use when asked to QA the stack, run a smoke test, verify a deployment, check stack health, check deploy readiness, or prove Slack tools, file upload/download, company context, logs, metrics, and tool loading work end to end."
---

# Centaur QA

Run this skill from a new Slack thread in a channel where the bot is present. The goal is to prove the user-facing agent path works, not just that APIs respond.

Default behavior: start immediately, run the in-thread smoke test, and return a concise pass/fail report. Do not ask clarifying questions unless the current Slack channel or thread cannot be inferred.

If the user asks for deploy readiness, staging QA, preview QA, promotion gating, concurrency checks, scheduler checks, or deadlock checks, run the in-thread smoke test first, then run the relevant extended checks below.

## Success Criteria

The smoke test passes only when all core workflows work from the running agent session:

- A list of tools loads and includes expected tools.
- Slack file upload to the current thread works.
- Slack file download from the current thread works.
- Slack file download from the current channel, then re-upload to the current thread, works.
- Slack token search works after bounded retries.
- Slack overall message search works.
- Current thread history works.
- Company context can connect to the Paradigm database and return indexed documents or a valid empty result.
- VictoriaLogs and VictoriaMetrics queries work.

Treat auth, permission, DNS, schema, and timeout errors as failures. Treat empty search results as warnings only when the tool successfully queried the backing service.

For promotion or deploy-readiness requests, the final status is `PASS` only if the requested extended checks also pass or are explicitly accepted by the owner.

## In-Thread Smoke Test

Use direct tool CLIs when available. Use `centaur-tools call <tool> <method> '<json>'` when a tool has no standalone CLI command for the method you need.

First capture session context:

```bash
echo "THREAD_KEY=${CENTAUR_THREAD_KEY:-}"
echo "SLACK_CHANNEL_ID=${SLACK_CHANNEL_ID:-}"
echo "SLACK_THREAD_TS=${SLACK_THREAD_TS:-}"
echo "SLACK_CHANNEL_NAME=${SLACK_CHANNEL_NAME:-}"
```

If `SLACK_CHANNEL_ID` or `SLACK_THREAD_TS` is missing, infer it from `CENTAUR_THREAD_KEY` when possible. Slack thread keys are usually `slack:<channel_id>:<thread_ts>`.

### 1. Tool Loading

```bash
centaur-tools list
```

Verify that the list is non-empty and includes at least `slack`, `company_context`, `vlogs`, and `vmetrics`. If a tool is absent, record the failure before continuing.

### 2. Upload File To Current Thread

Upload a tiny deterministic file to the current Slack thread:

```bash
QA_TOKEN="centaur-qa-$(date +%s)"
QA_B64=$(printf '%s\n' "$QA_TOKEN" | base64 | tr -d '\n')
centaur-tools call slack upload_file "{
  \"channel_id\": \"${SLACK_CHANNEL_ID}\",
  \"thread_ts\": \"${SLACK_THREAD_TS}\",
  \"filename\": \"centaur-qa-upload.txt\",
  \"title\": \"Centaur QA Upload\",
  \"comment\": \"QA upload smoke test: ${QA_TOKEN}\",
  \"content_base64\": \"${QA_B64}\"
}"
```

Verify the response contains a Slack file object, permalink, or URL. Save any `url_private` value for the next step. If the response omits file metadata, use thread history in step 4 to find the uploaded file.

### 3. Download File From Current Thread

Fetch the current thread history and find the uploaded file's `url_private`:

```bash
slack thread "${SLACK_CHANNEL_ID}:${SLACK_THREAD_TS}" --json --limit 20
```

Then download it to a sandbox-local temp directory:

```bash
QA_DOWNLOAD_DIR="/tmp/centaur-qa-files-${QA_TOKEN}"
mkdir -p "$QA_DOWNLOAD_DIR"
slack files "${URL_PRIVATE_FROM_CURRENT_THREAD}" --download --output "$QA_DOWNLOAD_DIR"
```

Verify the command prints a downloaded path and that the file exists with non-zero size.

### 4. Current Thread History

```bash
slack thread "${SLACK_CHANNEL_ID}:${SLACK_THREAD_TS}" --json --limit 20
```

Verify the returned messages include the QA request and the file upload message. Record the number of messages returned.

### 5. Search Uploaded Token

Search for the unique token uploaded in step 2:

```bash
for attempt in 1 2 3; do
  slack search "$QA_TOKEN" --limit 5 --full && break
  sleep 10
done
```

Verify at least one result points to the current channel or current thread. Retry up to three total attempts with 10 seconds between attempts to handle Slack indexing lag. If all three attempts complete successfully but return no matching result, record `WARN: token not indexed yet` and continue.

### 6. Search Overall Messages

Run a separate broader message search for a stable term from the current channel:

```bash
slack search "${SLACK_CHANNEL_NAME:-centaur}" --limit 5 --full
```

Pass when the command succeeds and returns valid search output. Empty results are a warning only when the query reached Slack successfully.

### 7. Download File From Current Channel And Re-Upload

Find another accessible Slack file from the current channel. Prefer a result outside the current thread, but do not use files from other channels. `search_files` filters by filename or title, so start broad with an empty query:

```bash
centaur-tools call slack search_files '{"query":"", "max_results":5}'
```

Pick a result whose `channels` includes `${SLACK_CHANNEL_ID}`. If possible, avoid the file uploaded earlier in this QA run so the check proves download-and-reupload of an existing channel file. Download it:

```bash
QA_REUPLOAD_DIR="/tmp/centaur-qa-reupload-${QA_TOKEN}"
mkdir -p "$QA_REUPLOAD_DIR"
slack files "${URL_PRIVATE_FROM_CURRENT_CHANNEL}" --download --output "$QA_REUPLOAD_DIR"
```

Save the downloaded file path, then re-upload it to the current thread:

```bash
slack upload "${SLACK_CHANNEL_ID}" "${DOWNLOADED_FILE_FROM_CURRENT_CHANNEL}" \
  --thread "${SLACK_THREAD_TS}" \
  --comment "QA re-upload from current channel"
```

Verify the current thread now shows the re-uploaded file. If no accessible current-channel file exists, record `SKIP: no readable file in current channel` and include the `search_files` result.

### 8. Paradigm DB Via Company Context

This verifies the database-backed context path and row-level permissions:

```bash
company_context list --limit 3 --json
company_context search "centaur" --limit 3 --json
```

Pass when the tool returns a valid JSON payload with `status: ok`, even if no documents match. Fail on database connection errors, permission errors, missing `COMPANY_CONTEXT_DSN`, or malformed results.

### 9. Logs Via VictoriaLogs

```bash
vlogs health
vlogs query '*' --limit 3 --json
vlogs query "centaur.thread_key:\"${CENTAUR_THREAD_KEY}\"" --limit 10 --json
```

Pass when VictoriaLogs is reachable and returns JSON log entries or a valid empty list for the thread-specific query. Fail on DNS, HTTP, or LogsQL errors.

### 10. Metrics Via VictoriaMetrics

`vmetrics` may not have a direct CLI, so use the tool bridge:

```bash
centaur-tools call vmetrics query '{"expr":"up"}'
centaur-tools call vmetrics series '{"match":"{__name__=~\".*\"}"}'
```

Pass when the responses are valid VictoriaMetrics API results. Fail on DNS, HTTP, or malformed response errors.

## Extended Checks

Run these after the core smoke test when the user asks for staging, preview, deploy readiness, scheduler, concurrency, or promotion confidence.

### Deployment Health

Record the target environment, namespace or URL, commit/build if visible, current timestamp, and thread key. Verify:

- The target is serving traffic.
- `centaur-tools list` succeeds from the running session.
- Recent `vlogs errors --start 1h` or equivalent log query has no new critical errors for the QA run.
- The user-visible Slack thread receives the final QA report.

### Concurrent Agent Turns

When asked to check concurrency or deadlocks, start 3-5 QA prompts in separate Slack threads in the same channel. Use distinct `QA_TOKEN` values and ask each agent to do a different read-only task:

- Read thread history and summarize earlier messages.
- Call two tools and summarize grounded results.
- Query logs for its own thread key.
- Search messages for a synthetic token.
- Use company context for a small internal DB lookup.

Pass when every turn reaches a terminal response in Slack, no thread remains stuck busy, and vlogs show one coherent execution per prompt without duplicate final delivery.

### User Context

Verify requester context when the user asks for Slack/user-context coverage:

- The response can refer to the current Slack channel and thread.
- The agent can identify or mention the requesting user only from available Slack context.
- Missing GitHub handles or profile fields are reported as unavailable, not invented.
- Mid-thread prompts use earlier thread facts accurately.

### Scheduler Checks

Run only when the target includes scheduler workflows, alerts, cron jobs, or background smoke loops. Use the scheduler's canonical workflow state, DB rows, or logs. Verify:

- The scheduler creates the expected current tick.
- It does not create a duplicate tick while a prior tick is pending or running.
- It does not backfill every missed tick when the last run is far in the past; it creates only the most recent eligible tick.

Pass only with evidence from scheduler-owned state and logs, not just absence of visible failures.

### Promotion Gate

A deployment is ready for promotion only when:

- The core in-thread smoke test passes.
- Requested extended checks pass or failures are explicitly accepted by the owner.
- The same commit/build was tested and is the one being promoted.
- The report includes enough evidence for another engineer to verify: Slack permalinks, thread key, execution IDs, workflow IDs, log query windows, or DB row counts.

## Report Format

Reply in the Slack thread with a digestible QA report, not a prose paragraph. The
first line must answer the outcome:

```text
Overall: PASS|FAIL|PARTIAL - <one short reason>
```

Use `PASS` only when every required smoke check passed. Use `PARTIAL` when the
user-facing path mostly worked but at least one required check warned, skipped,
or could not be verified. Use `FAIL` when any required check hit an auth,
permission, DNS, schema, timeout, malformed-response, upload/download, or
backing-service error.

Then include a Slack-friendly digest. Do not use Markdown tables in Slack
responses; Slack does not render them reliably. Use this exact shape:

```text
*Setup*
- *Thread context:* PASS - C123:1712345678.000000, key slack:C123:...
- *Tool loading:* PASS - 72 tools; expected slack/company_context/vlogs/vmetrics present

*Slack*
- *Upload current-thread file:* PASS - F123, centaur-qa-upload.txt
- *Download current-thread file:* PASS - centaur-qa-upload.txt, 22 bytes, token matched
- *Re-upload current-channel file:* SKIP - no readable prior file found in channel
- *Search uploaded token:* WARN - 3 attempts, 0 results; likely indexing lag
- *Search overall messages:* PASS - query "centaur", 5 results
- *Current thread history:* PASS - 4 messages; QA request and upload present

*Data + Observability*
- *Company context:* PASS - list 0, search 0, status ok
- *VictoriaLogs:* PASS - health ok, sample 3, thread query 0
- *VictoriaMetrics:* PASS - health ok, up query ok, series ok

*Extended*
- *Requested extended checks:* SKIP - not requested
```

Rules for the digest:

- Keep each evidence phrase to one short sentence fragment. Prefer concrete IDs,
  counts, filenames, byte counts, query names, and attempt counts.
- Do not write `mostly PASS`. Use `PARTIAL` and put each warning or skipped item
  on its own line.
- Do not paste raw JSON, stack traces, credentials, tokens, or long command
  output. Summarize the failure class and the command or endpoint that failed.
- If Slack file upload creates visible artifacts, mention the uploaded filenames
  and file IDs so the user can correlate them with thread attachments.
- If a check is skipped because no suitable input exists, mark only that row
  `SKIP`; do not hide it in prose.
- If using the Slack API directly, a Block Kit version is acceptable only when
  the message still contains the same information: headline, grouped sections,
  per-check result, and evidence. Do not require Block Kit for normal assistant
  replies.

After the grouped digest, add at most three short bullets:

- `Failures:` highest-signal failed rows and likely owner.
- `Warnings:` non-blocking warnings such as Slack indexing lag or empty-but-valid
  search/log results.
- `Promotion:` `ready`, `not ready`, or `not evaluated`, with one reason.

Omit any bullet that has no content. The whole report should fit comfortably in
one Slack message.

## Known Gotchas

| Symptom | Likely Cause | Action |
|---------|--------------|--------|
| Missing Slack env vars | Invocation did not come through Slack, or runtime metadata was not injected | Derive from `CENTAUR_THREAD_KEY`; otherwise fail early |
| `slack files --download` rejects URL | URL is not a Slack `url_private` file URL | Read thread history or `search_files` JSON and use `url_private` |
| Search cannot find just-uploaded token | Slack search indexing lag | Retry up to three total attempts, then warn and run the separate overall message search |
| `company_context` permission denied | Principal lacks DB-backed reader grant | Report principal/channel and ask owner to grant company context access |
| `vlogs` or `vmetrics` DNS failure | Observability service unavailable from sandbox | Check local stack or cluster service deployment |
| Expected tools missing | Tool catalog did not load or overlay masked base tools | Report the missing tool names and include `centaur-tools list` output |
| Concurrent runs hang | Runtime assignment, execution queue, or final delivery issue | Check execution state, vlogs thread trace, and delivery outbox |
| Scheduler duplicate or catch-up storm | Scheduler idempotency regression | Inspect scheduler-owned DB rows and logs before proposing a code fix |

## Failure Triage

When a flow fails, inspect runtime evidence before redesigning:

- Stuck execution: check execution state, `agent_execution_events`, and `vlogs thread_trace`.
- Missing Slack response: check Slackbot logs, final delivery state, and the Slack thread surface.
- File failure: check Slack file metadata, `url_private`, downloaded byte size, and upload response.
- Tool failure: classify credential, DNS, upstream, schema, timeout, or runtime errors separately.
- Context bug: inspect thread history, requester context, and message ordering.
- Scheduler bug: inspect scheduler-owned rows and logs for duplicate or catch-up decisions.

## References

| Reference | When To Read |
|-----------|--------------|
| [references/test-inputs.md](references/test-inputs.md) | When doing broader tool-by-tool QA beyond the smoke test |

## Templates

| Template | Purpose |
|----------|---------|
| [templates/tool-qa-report-template.md](templates/tool-qa-report-template.md) | Optional full QA report file for local stack runs |
