---
title: Slack ETL
description: Sync Slack channel history into Postgres, drain historical backfills, and project Slack context into searchable documents.
---

# Slack ETL

:::warning[Off by default in production]
Slack ETL is disabled unless the API service has `SLACK_ETL_ENABLED=true`.
Production deployments should enable it deliberately after choosing the Slack
token, channel scope, exclusion patterns, and data boundary they want agents to
use.
:::

Slack ETL keeps an indexed, queryable copy of public Slack history in Postgres
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
| `slack_sync` | 1 hour | Lists public channels, refreshes users, syncs recent root messages, advances per-channel checkpoints, and enqueues backfill jobs. |
| `slack_backfill` | 10 minutes | Claims queued backfill jobs and drains Slack cursors without slowing the incremental sync. |
| `company_context_documents` | 4 hours | Projects changed Slack rows into `company_context_documents` for retrieval. |

The schedules are registered from the workflow files at API startup. Each
workflow uses `no_delivery`, so scheduled runs write to the database without
posting to Slack.

## Configure Slack access

Create a Slack user token for ETL reads and store it as `SLACK_ETL_TOKEN` in
the same secret source used by tools. The Slack tool declares it as an optional
HTTP secret for `slack.com`; iron-proxy injects the real value when the tool
calls Slack.

The token must be able to call:

| Slack API | Used for |
|-----------|----------|
| `conversations.list` | Discover public channels. |
| `conversations.history` | Read channel root messages. |
| `conversations.replies` | Refresh thread replies. |
| `users.list` | Resolve Slack user metadata for documents. |

Slack ETL currently syncs public channels visible to the configured ETL user
token. It does not sync private channels, DMs, or Slackbot-only live thread
events.

## Enable the schedules

Set `SLACK_ETL_ENABLED=true` on the API service. The other schedules default on
once Slack ETL is enabled, but can be tuned independently.

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
| `SLACK_ETL_EXCLUDED_CHANNEL_PATTERNS` | empty | Comma-separated channel-name globs to skip, without needing the leading `#`. |
| `COMPANY_CONTEXT_DOCUMENTS_ENABLED` | `true` | Enables projection from Slack sync rows into company context documents. |
| `COMPANY_CONTEXT_DOCUMENTS_INTERVAL_SECONDS` | `14400` | How often to project changed Slack rows into documents. |

Example exclusion list:

```bash
SLACK_ETL_EXCLUDED_CHANNEL_PATTERNS="#eng-*-alerts,*-monitor-*"
```

## Data model

Slack ETL writes normalized Slack data into dedicated tables:

| Table | Contents |
|-------|----------|
| `slack_sync_channels` | Public channels visible to the ETL token and whether they are currently syncable. |
| `slack_sync_users` | Slack user display metadata used when rendering documents. |
| `slack_sync_runs` | One row per incremental or backfill workflow run, with counts and channel outcomes. |
| `slack_sync_messages` | Root messages and replies keyed by `(channel_id, message_ts)`. |
| `slack_sync_checkpoints` | Per-channel watermarks and last error state. |
| `slack_sync_backfill_jobs` | Deferred channel-history and thread-refresh jobs. |
| `company_context_documents` | Derived channel-day and thread documents for retrieval. |

The first incremental run reads a small recent window so useful data appears
quickly, then seeds historical backfill jobs for the configured lookback. Later
incremental runs resume from each channel checkpoint and re-read a trailing
thread window so recent edits and replies are picked up.

The lookback values are read windows, not retention windows. Lowering
`SLACK_SYNC_BACKFILL_LOOKBACK_DAYS` or `SLACK_SYNC_THREAD_LOOKBACK_DAYS` limits
future backfill and refresh work, but it does not delete Slack rows or company
context documents that were already synced.

## Run it manually

Use a manual run when enabling the feature or testing a configuration change.
From inside the API deployment, localhost bypass avoids needing an external API
key:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s -X POST \
  http://localhost:8000/workflows/runs \
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

kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s \
  "http://localhost:8000/workflows/runs/${RUN_ID}" | jq
```

To drain pending historical work immediately:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s -X POST \
  http://localhost:8000/workflows/runs \
  -H "Content-Type: application/json" \
  -d '{
    "workflow_name": "slack_backfill",
    "input": {"channel_batch_limit": 10},
    "eager_start": true
  }' | jq
```

To force document projection after rows have synced:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api -- curl -s -X POST \
  http://localhost:8000/workflows/runs \
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
kubectl exec -n centaur deploy/centaur-centaur-api -- \
  psql "$DATABASE_URL" -c \
  "SELECT schedule_id, workflow_name, enabled, interval_seconds, next_run_at
   FROM workflow_schedules
   WHERE schedule_id IN ('slack_sync', 'slack_backfill', 'company_context_documents')
   ORDER BY schedule_id;"
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
| Schedules exist but are disabled | Confirm `SLACK_ETL_ENABLED=true` is present in the API environment. |
| `slack_sync` skips with `no_public_channels` | Confirm the ETL user token can see the expected public channels. |
| Channels are all skipped | Check `SLACK_ETL_EXCLUDED_CHANNEL_PATTERNS` for broad globs. |
| Checkpoints show `missing_scope` or `not_allowed_token_type` | Add the missing Slack OAuth scope or use the expected user-token class. |
| Backfill jobs keep failing | Inspect `slack_sync_backfill_jobs.last_error` and the corresponding `slack_sync_runs` row. |
| Documents lag behind messages | Check the `company_context_documents` workflow status and `company_context_projection_lag_seconds`. |

Keep the ETL token scoped to the channels and workspace data you actually want
agents to retrieve. Synced rows and projected documents are deployment-wide
context, so treat the token as a deliberate data boundary.
