---
name: qa
description: "Smoke test a running Centaur deployment from a new Slack thread. Use when asked to QA the stack, run a smoke test, verify a deployment, check deploy readiness, or prove Slack connectors, file upload/download, company context, logs, metrics, and connector loading work end to end."
---

# Centaur QA

Run this skill from a new Slack thread in a channel where the bot is present. The goal is to prove the user-facing agent path works, not just that APIs respond.

Default behavior: start immediately, run the in-thread smoke test, and return a concise pass/fail report. Do not ask clarifying questions unless the current Slack channel or thread cannot be inferred.

If the user asks for deploy readiness, promotion gating, concurrency checks, or broad tool coverage, run the in-thread smoke test first, then the relevant extended checks below.

## Success Criteria

The smoke test passes only when all core workflows work from the running agent session:

- The connector list loads and includes expected connectors.
- Slack file upload to the current thread works.
- Slack file download from the current thread works.
- Slack file download from the current channel, then re-upload to the current thread, works.
- Slack token search works after bounded retries.
- Slack overall message search works.
- Current thread history works.
- Company context search reaches its API and returns indexed documents or a valid empty result.
- VictoriaLogs and VictoriaMetrics are reachable, and VictoriaMetrics proves metric existence without asserting exact metric values.

Treat auth, permission, DNS, schema, and timeout errors as failures. Treat empty search results as warnings only when the tool successfully queried the backing service.

For promotion or deploy-readiness requests, the final status is `PASS` only if the requested extended checks also pass or are explicitly accepted by the owner.

## In-Thread Smoke Test

Run every check through Code Mode connectors (`centaur-codemode exec --result-only`), the same path the agent uses for real work. `centaur-codemode load <connector>` prints each connector's functions and arguments.

Slack file transfers must go through the Centaur API server Slack proxy. In a Slack channel session, `tools.slack.get_access()` returns `access: "centaur"`, and `slack.upload_file` and `slack.download_file` then route through `/api/slack/files/...`. If `access` is anything else, record a FAIL for the file checks: the session is not on the user-facing path. Do not use raw `url_private` fetches.

Connectors read and write files only under the home directory, so keep QA files there, not in `/tmp`.

First capture session context:

```bash
echo "THREAD_KEY=${CENTAUR_THREAD_KEY:-}"
echo "SLACK_CHANNEL_ID=${SLACK_CHANNEL_ID:-}"
echo "SLACK_THREAD_TS=${SLACK_THREAD_TS:-}"
echo "SLACK_CHANNEL_NAME=${SLACK_CHANNEL_NAME:-}"
```

If `SLACK_CHANNEL_ID` or `SLACK_THREAD_TS` is missing, infer it from `CENTAUR_THREAD_KEY` when possible. Slack thread keys are usually `slack:<channel_id>:<thread_ts>`. Substitute the values into the cells below.

### 1. Connector Loading

```bash
centaur-codemode list
```

Verify that the list is non-empty and includes at least `slack`, `company_context`, `vlogs`, and `vmetrics`. If a connector is absent, record the failure before continuing.

### 2. Upload File To Current Thread

Write a tiny deterministic file under the home directory:

```bash
QA_TOKEN="centaur-qa-$(date +%s)"
QA_DIR="$HOME/centaur-qa/${QA_TOKEN}"
mkdir -p "$QA_DIR"
printf '%s\n' "$QA_TOKEN" > "$QA_DIR/centaur-qa-upload.txt"
echo "$QA_TOKEN $QA_DIR"
```

Upload it to the current thread through the proxy:

```js
const access = await tools.slack.get_access({});
const upload = await tools.slack.upload_file({
  channel: "SLACK_CHANNEL_ID",
  thread_ts: "SLACK_THREAD_TS",
  path: "QA_DIR/centaur-qa-upload.txt",
  initial_comment: "QA upload smoke test: QA_TOKEN",
});
return { access, upload };
```

Verify `access.access` is `centaur` and `upload.file_ids` has a Slack file ID (`F...`). Save it for the next step. If none is returned, use thread history in step 3 to find the uploaded file's `id`.

### 3. Download File From Current Thread

If step 2 returned no file ID, take it from the current thread's messages (`files[].id`):

```js
return await tools.slack.thread_replies({ channel: "SLACK_CHANNEL_ID", ts: "SLACK_THREAD_TS", limit: 20 });
```

Download it through the proxy:

```js
return await tools.slack.download_file({
  file: "FILE_ID_FROM_CURRENT_THREAD",
  channel: "SLACK_CHANNEL_ID",
  path: "QA_DIR/download/centaur-qa-upload.txt",
});
```

Verify the result has a `path` and non-zero `bytes`, and that the file contains the token:

```bash
grep -q "$QA_TOKEN" "$QA_DIR/download/centaur-qa-upload.txt" && echo token-matched
```

### 4. Current Thread History

```js
const { messages } = await tools.slack.thread_replies({ channel: "SLACK_CHANNEL_ID", ts: "SLACK_THREAD_TS", limit: 20 });
return { count: messages.length, messages: messages.map(({ ts, text, files }) => ({ ts, text: text.slice(0, 200), files })) };
```

Verify the returned messages include the QA request and the file upload message. Record the number of messages returned.

### 5. Search Uploaded Token

Search for the unique token uploaded in step 2, retrying for indexing lag:

```js
const access = await tools.slack.get_access({});
const attempts = [];
for (let attempt = 1; attempt <= 3; attempt++) {
  const result = await tools.slack.search_messages({ query: "QA_TOKEN", count: 5 });
  attempts.push(result);
  if (JSON.stringify(result).includes("SLACK_CHANNEL_ID")) break;
  if (attempt < 3) await sleep(10000);
}
return { search_backend: access.search, attempts };
```

Verify at least one result points to the current channel or current thread. If all three attempts succeed but return no matching result, record `WARN: token not indexed yet` and continue. Record `search_backend`: with `company_context`, search reads the company-context index, which lags Slack more than native search.

### 6. Search Overall Messages

Run a separate broader message search for a stable term from the current channel:

```js
return await tools.slack.search_messages({ query: "SLACK_CHANNEL_NAME", count: 5 });
```

Use `centaur` as the query when `SLACK_CHANNEL_NAME` is unset.

Pass when the call succeeds and returns valid search output. Empty results are a warning only when the query reached the backing service successfully.

### 7. Download File From Current Channel And Re-Upload

Find another accessible Slack file from the current channel's recent messages, including thread replies: in many channels every file sits in a reply, so top-level messages alone often find nothing. Skip the current thread and the `centaur-qa-upload.txt` files earlier QA runs leave behind, so the check proves download-and-reupload of an existing channel file:

```js
const { messages } = await tools.slack.channel_history({ channel: "SLACK_CHANNEL_ID", limit: 200 });
const toFiles = (m) => (m.files ?? []).map((f) => ({ ts: m.ts, thread_ts: m.thread_ts, id: f.id, name: f.name, size: f.size }));
const keep = (f) => f.name !== "centaur-qa-upload.txt";
const others = messages.filter((m) => m.ts !== "SLACK_THREAD_TS");
const found = others.flatMap(toFiles).filter(keep);
// Then scan thread replies, newest threads first, in batches of 8 (the Code Mode concurrency limit).
const threads = others.filter((m) => (m.reply_count ?? 0) > 0);
for (let i = 0; i < threads.length && found.length < 5; i += 8) {
  const batch = await Promise.allSettled(
    threads.slice(i, i + 8).map((t) => tools.slack.thread_replies({ channel: "SLACK_CHANNEL_ID", ts: t.ts, limit: 200 })),
  );
  for (const r of batch) {
    if (r.status !== "fulfilled") continue;
    // The first message is the parent, already counted above.
    found.push(...r.value.messages.slice(1).flatMap(toFiles).filter(keep));
  }
}
return { top_level: messages.length, threads_with_replies: threads.length, files: found.slice(0, 5) };
```

Download it through the proxy and re-upload it to the current thread in one cell:

```js
const downloaded = await tools.slack.download_file({
  file: "FILE_ID_FROM_CURRENT_CHANNEL",
  channel: "SLACK_CHANNEL_ID",
  path: "QA_DIR/reupload/FILE_NAME",
});
const uploaded = await tools.slack.upload_file({
  channel: "SLACK_CHANNEL_ID",
  thread_ts: "SLACK_THREAD_TS",
  path: downloaded.path,
  initial_comment: "QA re-upload from current channel",
});
return { downloaded: { path: downloaded.path, bytes: downloaded.bytes }, uploaded };
```

Verify the current thread now shows the re-uploaded file. If no accessible file exists in the channel's top-level messages or thread replies, record `SKIP: no readable file in current channel` and include the `top_level` and `threads_with_replies` counts.

### 8. Company Context

This verifies the company-context API path and per-requester visibility:

```js
return await tools.company_context.search({ query: "centaur", limit: 3 });
```

Pass when the call returns a valid `results` array, even if empty. Fail on connection, auth, or permission errors, a missing service URL, or malformed results.

If company context fails, use runtime evidence before suggesting a code fix:

```js
const [thread, upstream] = await Promise.allSettled([
  tools.vlogs.thread_logs({ thread_key: "THREAD_KEY", start: "2h" }),
  tools.vlogs.query({ query: '_time:2h ("upstream connection failed" OR "failed to fetch secret" OR company_context)', limit: 20 }),
]);
return { thread, upstream };
```

Classify whether the failure is a service or proxy upstream issue, secret-resolution failure, or connector schema/client error. Include the failing call and thread key in the report.

### 9. Logs Via VictoriaLogs

```js
const [ready, sample, thread] = await Promise.allSettled([
  tools.vlogs.ready({}),
  tools.vlogs.query({ query: "*", limit: 3 }),
  tools.vlogs.query({ query: 'centaur.thread_key:"THREAD_KEY"', limit: 10 }),
]);
return { ready, sample, thread };
```

Pass when VictoriaLogs is reachable and returns log entries or a valid empty list for the current-thread query. Fail on DNS, HTTP, or LogsQL errors. Keep this check scoped to the current deployment state visible from the current thread.

Do not run broad error searches such as `level:error`, `vlogs.errors`, or `error OR ERROR` as part of the default QA pass/fail decision. The goal of this step is to prove the vlogs connection works, not to audit whether unrelated services currently have errors. Only query error logs when a preceding check failed and you need runtime evidence to classify that specific failure.

### 10. Metrics Via VictoriaMetrics

```js
const [health, count, series] = await Promise.allSettled([
  tools.vmetrics.health({}),
  tools.vmetrics.query({ expr: "count(up)" }),
  tools.vmetrics.series({ match: "up", limit: 5 }),
]);
return { health, count, series };
```

Pass when VictoriaMetrics is reachable and the responses prove that at least one `up` series exists. Do not use catch-all selectors such as `{__name__=~".+"}`; they scan every series and are too expensive on shared environments. Do not assert exact sample values, counter values, gauge values, label values, or a specific `up` value; those are environment-dependent. Fail on DNS, HTTP, malformed response errors, or a valid response that proves no metric names or series exist.

## Extended Checks

Run these after the core smoke test when the user asks for deploy readiness, concurrency, broad tool coverage, or promotion confidence.

### Tool Health

For broad tool coverage, follow the `tool-health-smoke` skill. It runs `<tool> health` for every live tool CLI through the normal brokered-credential path. Use `health` as the canonical tool smoke surface; only run other read-only commands after recording a failed health result, to classify that failure.

### Concurrent Agent Turns

When asked to check concurrency or deadlocks, start 3-5 QA prompts in separate Slack threads in the same channel. Use distinct `QA_TOKEN` values and ask each agent to do a different read-only task:

- Read thread history and summarize earlier messages.
- Call two tools and summarize grounded results.
- Query logs for its own thread key.
- Search messages for a synthetic token.
- Use company context for a small internal DB lookup.

Pass when every turn reaches a terminal response in Slack, no thread remains stuck busy, and vlogs show one coherent execution per prompt without duplicate final delivery.

### Promotion Gate

A deployment is ready for promotion only when:

- The core in-thread smoke test passes.
- Requested extended checks pass or failures are explicitly accepted by the owner.
- The same commit/build was tested and is the one being promoted.
- The report includes enough evidence for another engineer to verify: Slack permalinks, thread key, execution IDs, and log query windows.

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
- *Connector loading:* PASS - 72 connectors; expected slack/company_context/vlogs/vmetrics present

*Slack*
- *Upload current-thread file:* PASS - F123, centaur-qa-upload.txt
- *Download current-thread file:* PASS - centaur-qa-upload.txt, 22 bytes, token matched
- *Re-upload current-channel file:* SKIP - no readable prior file found in channel
- *Search uploaded token:* WARN - 3 attempts, 0 results; likely indexing lag
- *Search overall messages:* PASS - query "centaur", 5 results
- *Current thread history:* PASS - 4 messages; QA request and upload present

*Data + Observability*
- *Company context:* PASS - search 0 results, valid response
- *VictoriaLogs:* PASS - ready, sample 3, thread query 0
- *VictoriaMetrics:* PASS - reachable, `up` series found

*Extended*
- *Tool health:* SKIP - not requested
- *Concurrency:* SKIP - not requested
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

After the grouped digest, add at most three short bullets. Use Slack mrkdwn list items with `- ` prefixes:

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
| `slack.download_file` rejects arguments | Passed a URL or permalink instead of a file ID, or the channel is not where the file is shared | Read `files[].id` (`F...`) from thread or channel history and pass the channel ID it is shared in |
| `slack.upload_file` or `download_file` says no such file or path outside the files root | QA file written outside the home directory | Write QA files under `$HOME` |
| `slack.get_access()` is not `centaur` | Session lacks Centaur API Slack context | Fail the file checks and report the thread key and access value |
| Search cannot find just-uploaded token | Slack search indexing lag | Retry up to three total attempts, then warn and run the separate overall message search |
| `company_context.search` permission denied | Principal lacks company context access | Report principal/channel and ask owner to grant company context access |
| `company_context.search` connection failed | Service, proxy upstream, or secret-resolution failure | Check thread logs and vlogs for upstream connect and secret fetch errors before proposing a code change |
| Tool CLI import error | Package entrypoint or relative import packaging regression | Run `<tool> health`, `<tool> --help`, package build checks, and report the broken console script |
| Tool crashes when an API key env var is absent | Client assumes local env auth despite brokered credentials | Re-run `<tool> health` with the env var unset and verify client construction does not raise |
| Tool returns 401/403 | Missing grant, wrong secret, or header not injected by iron-proxy | Check the principal's grants and the tool's `[tool.centaur]` hosts/match rules, then follow the `auth-failure-log-triage` skill |
| `vlogs` or `vmetrics` DNS failure | Observability service unavailable from sandbox | Check local stack or cluster service deployment |
| Expected connectors missing | Connector catalog did not load, policy hides it, or an overlay shadowed a base connector | Report the missing connector names and include `centaur-codemode list` output |
| Concurrent runs hang | Runtime assignment, execution queue, or final delivery issue | Check `session_executions`, `session_events`, and the vlogs thread trace |

## Failure Triage

When a flow fails, inspect runtime evidence before redesigning:

- Stuck execution: check `session_executions`, `session_events`, and `tools.vlogs.thread_trace({ thread_key })`.
- Missing Slack response: check Slackbot logs, final delivery state, and the Slack thread surface.
- File failure: check `slack.get_access()`, the downloaded byte size, the proxied upload response, and the `requests` the cell recorded (run without `--result-only`).
- Connector failure: classify credential, DNS, upstream, schema, timeout, or runtime errors separately.
- Brokered-auth failure: verify whether the tool should work without local env vars and whether the proxy injected the expected headers.
- Context bug: inspect thread history, requester context, and message ordering.

## References

| Reference | When To Read |
|-----------|--------------|
| [../tool-health-smoke/SKILL.md](../tool-health-smoke/SKILL.md) | Broad all-tool health coverage |
| [../auth-failure-log-triage/SKILL.md](../auth-failure-log-triage/SKILL.md) | A tool fails with an auth or permission error |
| [references/test-inputs.md](references/test-inputs.md) | Only for deeper read-only triage after a tool health check fails |
