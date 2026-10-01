---
name: centaur-observability
description: "Investigate Centaur runtime behavior with VictoriaLogs (`vlogs`) and VictoriaMetrics (`vmetrics`). Use when a user reports something broken or stalled, a workflow, alert, or channel post never populated, asks to check code for issues, or needs thread, execution, sandbox, tool-call, service-health, or deploy evidence."
---

# Centaur Observability

Investigate runtime evidence before proposing redesigns, simplifications, or config rewiring. Skip this skill if the sandbox prompt says observability access is disabled.

## Workflow

1. Identify the affected surface: thread key, execution id, service, tool, and rough time window.
2. Read the relevant code paths and workflow status alongside the logs.
3. Query the narrowest matching evidence below. Run independent queries in parallel.
4. Report a root cause when established, or bounded hypotheses plus the evidence needed to confirm them. Offer redesign advice only after findings, or when the user asks to skip investigation.

For auth, credential, or proxy failures, use the `auth-failure-log-triage` skill.

## Logs

```bash
centaur-tools call vlogs errors '{"start":"1h"}'
centaur-tools call vlogs errors '{"service":"api","start":"6h"}'
centaur-tools call vlogs thread_logs '{"thread_key":"slack:T1:C1:1234","start":"24h"}'
centaur-tools call vlogs thread_trace '{"thread_key":"slack:T1:C1:1234"}'
centaur-tools call vlogs execution_timeline '{"execution_id":"exe_123"}'
centaur-tools call vlogs slow_requests '{"threshold_ms":3000}'
centaur-tools call vlogs tool_calls '{"tool_name":"websearch","start":"24h"}'
centaur-tools call vlogs tool_analytics '{"start":"7d"}'
centaur-tools call vlogs service_health '{"start":"1h"}'
centaur-tools call vlogs sandbox_activity '{"start":"1h"}'
vlogs query 'level:error AND event:tool_call_completed' --limit 20
```

## Metrics

```bash
centaur-tools call vmetrics query '{"expr":"centaur_deployment_info"}'                 # component, version, git_sha, image
centaur-tools call vmetrics query '{"expr":"centaur_last_deploy_timestamp_seconds"}'   # last deploy per component
centaur-tools call vmetrics query '{"expr":"centaur_overlay_revision_info"}'           # deployed overlay revisions
centaur-tools call vmetrics query '{"expr":"centaur_overlay_revision_scrape_success"}' # overlay scrape health
```

Run `vlogs --help` or `vmetrics --help` for other commands. If a query fails or returns nothing, say so; do not substitute repo files, values files, or memory for live state.
