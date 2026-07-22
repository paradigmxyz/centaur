---
title: Slack ETL
description: Sync Slack channel history into Postgres, drain historical backfills, and project Slack context into searchable documents.
---

# Slack ETL

:::warning[Off by default in production]
Slack ETL is disabled unless Helm values set `apiRs.etl.slack.enabled=true`.
Production deployments should enable it deliberately after choosing the Slack
token, channel scope, exclusion patterns, and data boundary they want agents to
use.
:::

Slack ETL keeps an indexed, queryable copy of Slack channel history in Postgres
for agent context and operator workflows. It runs as scheduled Centaur
workflows: one workflow keeps recent channel history fresh, one drains deferred
historical backfill work, and one turns synced messages into company context
documents. See [Creating Workflows](/extend/workflows) for the durable workflow
model behind these jobs.

The ETL path is separate from Slackbot delivery. Slackbot handles live user
turns in Slack threads; Slack ETL reads Slack history with a dedicated user
token and writes durable rows into Postgres.

## What it runs

| Workflow | Default cadence | Role |
|----------|-----------------|------|
| `slack_sync` | 1 hour | Lists channels, refreshes users, syncs recent root messages, advances per-channel checkpoints, and enqueues backfill jobs. |
| `slack_backfill` | 10 minutes | Claims queued backfill jobs and drains Slack cursors without slowing the incremental sync. |
| `company_context_documents` | 4 hours | Projects changed Slack rows into `company_context_documents` for retrieval. |

The schedules are registered from the workflow files at API startup. Each
workflow uses `no_delivery`, so scheduled runs write to the database without
posting to Slack.

## Configure Slack access

Create a Slack user token for ETL reads and store it as `SLACK_ETL_TOKEN` in
the same secret source used by tools. The Slack tool declares it as an optional
HTTP secret for `slack.com` and `files.slack.com`; iron-proxy injects the real
value when the tool calls Slack.

The token must be able to call:

| Slack API | Used for |
|-----------|----------|
| `conversations.list` | Discover public channels, and private channels when explicitly enabled. |
| `conversations.history` | Read channel root messages. |
| `conversations.replies` | Refresh thread replies. |
| `users.list` | Resolve Slack user metadata for documents. |
| `files:read` / file URL access | Download message attachment bytes from `files.slack.com`. |

Slack ETL syncs public channels visible to the configured ETL user token.
Set `SLACK_SYNC_INDEX_PRIVATE_CHANNELS=true` to also sync private channels
visible to that token. It does not sync DMs or Slackbot-only live thread events.
Private channel rows are protected by RLS: `centaur_readonly` sees public
channel data and the channel in `centaur.slack_channel_id`.

### User-scoped private-channel ingestion

The console's Slack OAuth flow can ingest private channels through the same
user-scoped pipeline used for DMs. Add `groups:read` and `groups:history` to the
Slack OAuth app's allowed user scopes and have existing users consent again.
DM-only credentials continue syncing DMs while credentials with the new scopes
also request `private_channel` conversations.

Every 10 minutes the console worker fans out across healthy Slack broker
credentials. Each credential lists the private channels visible to that user,
reads channel history, and fetches the complete `conversations.members` list.
Messages are deduplicated by Slack conversation and message timestamp when
multiple credentials can see the same channel.

User-scoped private channels are stored with DMs and MPIMs in the neutral
`slack_private_sync_*` and `slack_private_*_context_documents` tables rather than
`company_context_documents`. RLS checks `(team_id, channel_id, user_id)` against
the reconciled membership list. A successful sync marks members omitted from
Slack's complete member list inactive; a partial or truncated member list is
never applied.

## Enable the schedules

Set `apiRs.etl.slack.enabled=true` in Helm values. The chart renders the
corresponding API and workflow-host env automatically; do not set
`SESSION_SANDBOX_PASSTHROUGH_ENV` by hand for these ETLs. The other schedules
default on once Slack ETL is enabled, but can be tuned independently.

```yaml
apiRs:
  etl:
    slack:
      enabled: true
```

| Environment variable | Default | Effect |
|----------------------|---------|--------|
| `SLACK_ETL_ENABLED` | `false` | Enables `slack_sync`, `slack_backfill`, and the default document projection. |
| `SLACK_SYNC_INTERVAL_SECONDS` | `3600` | How often to run incremental Slack sync. |
| `SLACK_BACKFILL_ENABLED` | `true` | Enables the backfill worker schedule. |
| `SLACK_BACKFILL_INTERVAL_SECONDS` | `600` | How often to drain queued backfill jobs. |
| `SLACK_BACKFILL_CHANNEL_BATCH_LIMIT` | `50` | Maximum backfill jobs claimed per run. |
| `SLACK_BACKFILL_CHANNEL_PAGES_PER_JOB` | `5` | Maximum Slack history pages drained before a job is requeued. |
| `SLACK_SYNC_BACKFILL_LOOKBACK_DAYS` | `30` | Historical window seeded for first-time channel backfills. |
| `SLACK_SYNC_THREAD_LOOKBACK_DAYS` | `3` | Recent thread window eligible for reply refresh. |
| `SLACK_SYNC_INDEX_PRIVATE_CHANNELS` | `false` | Includes private channels visible to the ETL token in Slack sync and backfill. |
| `SLACK_ETL_ATTACHMENTS_ENABLED` | `true` | Download Slack message attachment bytes into Postgres. Metadata rows are still written when downloads are disabled. |
| `SLACK_ETL_ATTACHMENT_MAX_BYTES` | `10485760` | Per-file byte cap for Slack attachment downloads. Oversized files keep metadata with `skipped_too_large` status. |
| `SLACK_ETL_EXCLUDED_CHANNEL_PATTERNS` | empty | Comma-separated channel-name globs to skip, without needing the leading `#`. |
| `SLACK_RETENTION_ENABLED` | `true` | Allows the `slack_retention` schedule to run when at least one Slack retention TTL is positive. |
| `SLACK_RETENTION_INTERVAL_MINUTES` | `60` | How often to prune Slack retention-managed rows. |
| `SLACK_ETL_RETENTION_DAYS` | `0` | Deletes Slack ETL messages, derived Slack documents, and terminal ETL run/job rows older than this many days. `0` disables ETL retention. |
| `SLACK_DM_RETENTION_DAYS` | `0` | Deletes user-scoped private Slack messages, stale empty conversations, and terminal run/job rows older than this many days. `0` disables retention. |
| `COMPANY_CONTEXT_DOCUMENTS_ENABLED` | `true` | Enables projection from Slack sync rows into company context documents. |
| `COMPANY_CONTEXT_DOCUMENTS_INTERVAL_SECONDS` | `14400` | How often the coordinator claims stale projection scopes. |
| `COMPANY_CONTEXT_DOCUMENTS_MAX_WINDOW_SECONDS` | `21600` | Maximum source `updated_at` window claimed for one scope before it advances its watermark. |
| `COMPANY_CONTEXT_DOCUMENTS_BATCH_SIZE` | `50` | Maximum changed source rows processed by one per-scope child workflow. |

Example exclusion list:

```bash
SLACK_ETL_EXCLUDED_CHANNEL_PATTERNS="#eng-*-alerts,*-monitor-*"
```

## Data model

Slack ETL writes normalized Slack data into dedicated tables:

| Table | Contents |
|-------|----------|
| `slack_sync_channels` | Channels visible to the ETL token, channel privacy, and whether they are currently syncable. |
| `slack_sync_users` | Slack user display metadata used when rendering documents. |
| `slack_sync_runs` | One row per incremental or backfill workflow run, with counts and channel outcomes. |
| `slack_sync_messages` | Root messages and replies keyed by `(channel_id, message_ts)`. |
| `slack_sync_message_attachments` | Slack files attached to synced root messages and replies, including metadata, download status, checksum, and bounded `bytea` content when fetched. |
| `slack_sync_checkpoints` | Per-channel watermarks and last error state. |
| `slack_sync_backfill_jobs` | Deferred channel-history and thread-refresh jobs. |
| `company_context_documents` | Derived channel-day, thread, and attachment-metadata documents for retrieval. |

Attachment document projection indexes Slack file names, titles, MIME/file
types, Slack permalinks, download status, checksums, and the message the file
was attached to. It does not parse attachment bytes or index private Slack
download URLs.

The first incremental run reads a small recent window so useful data appears
quickly, then seeds historical backfill jobs for the configured lookback. Later
incremental runs resume from each channel checkpoint and re-read a trailing
thread window so recent edits and replies are picked up.

The lookback values are read windows, not retention windows. Lowering
`SLACK_SYNC_BACKFILL_LOOKBACK_DAYS` or `SLACK_SYNC_THREAD_LOOKBACK_DAYS` limits
future backfill and refresh work, but it does not delete Slack rows or company
context documents that were already synced.

Retention is handled by the separate `slack_retention` workflow. Public Slack
ETL and Slack DM data have independent TTLs so deployments can keep DM data for
a shorter period than channel ETL data. The workflow only runs when at least one
TTL is positive.

## Run it manually

Use a manual run when enabling the feature or testing a configuration change.
From inside the API deployment, localhost bypass avoids needing an external API
key:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s -X POST \
  http://localhost:8080/api/workflows/runs \
  -H "Content-Type: application/json" \
  -d '{
    "workflow_name": "slack_sync",
    "input": {"metadata": {"reason": "manual_check"}},
    "eager_start": true
  }' | jq
```

Then inspect the run:

```bash
RUN_ID=wfr_...

kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s \
  "http://localhost:8080/api/workflows/runs/${RUN_ID}" | jq
```

To drain pending historical work immediately:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s -X POST \
  http://localhost:8080/api/workflows/runs \
  -H "Content-Type: application/json" \
  -d '{
    "workflow_name": "slack_backfill",
    "input": {"channel_batch_limit": 10},
    "eager_start": true
  }' | jq
```

To force document projection after rows have synced:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s -X POST \
  http://localhost:8080/api/workflows/runs \
  -H "Content-Type: application/json" \
  -d '{
    "workflow_name": "company_context_documents",
    "input": {},
    "eager_start": true
  }' | jq
```

## Verify

Check the workflow schedules:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s \
  http://localhost:8080/api/workflows/schedules | jq \
  '.schedules[]
   | select(.schedule_id == "slack_sync"
     or .schedule_id == "slack_backfill"
     or .schedule_id == "company_context_documents")
   | {schedule_id, workflow_name, enabled, interval_seconds}'
```

Check recent workflow runs:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- curl -s \
  "http://localhost:8080/api/workflows/runs?limit=20" | jq \
  '.runs[]
   | select(.workflow_name == "slack_sync"
     or .workflow_name == "slack_backfill"
     or .workflow_name == "company_context_documents")
   | {workflow_name, status, created_at, attempts}'
```

Check sync health:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api -- \
  psql "$DATABASE_URL" -c \
  "SELECT channel_id, watermark_ts, last_success_at, last_error
   FROM slack_sync_checkpoints
   ORDER BY updated_at DESC
   LIMIT 20;"
```

Check backfill pressure:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api -- \
  psql "$DATABASE_URL" -c \
  "SELECT job_type, status, count(*), min(updated_at) AS oldest_updated_at
   FROM slack_sync_backfill_jobs
   GROUP BY job_type, status
   ORDER BY job_type, status;"
```

Check document projection:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api -- \
  psql "$DATABASE_URL" -c \
  "SELECT source_type, count(*), max(source_updated_at)
   FROM company_context_documents
   WHERE source = 'slack'
   GROUP BY source_type
   ORDER BY source_type;"
```

Centaur also exports ETL metrics, including cursor lag, sync freshness, active
and failed scopes, backfill job counts and age, item counters, document change
counters, and Slack projection lag. Use those alongside `slack_sync_runs` when
setting alerts.

## Troubleshoot

| Symptom | What to check |
|---------|---------------|
| Schedules are missing | Confirm `WORKFLOW_DIRS` includes `/app/workflows` and the API restarted after the workflow files were deployed. |
| Schedules exist but are disabled | Confirm Helm values set `apiRs.etl.slack.enabled=true` and the API pod was restarted. |
| `slack_sync` skips with `no_channels` | Confirm the ETL user token can see the expected public channels, or enable private channel sync when only private channels are in scope. |
| Channels are all skipped | Check `SLACK_ETL_EXCLUDED_CHANNEL_PATTERNS` for broad globs. |
| Checkpoints show `missing_scope` or `not_allowed_token_type` | Add the missing Slack OAuth scope or use the expected user-token class. |
| Backfill jobs keep failing | Inspect `slack_sync_backfill_jobs.last_error` and the corresponding `slack_sync_runs` row. |
| Documents lag behind messages | Check `company_context_projection_checkpoints` for an expired lease or old watermark, then inspect the per-scope `company_context_documents` child workflow and `company_context_projection_lag_seconds`. |

Keep the ETL token scoped to the channels and workspace data you actually want
agents to retrieve. Synced rows and projected documents are deployment-wide
context, so treat the token as a deliberate data boundary.
