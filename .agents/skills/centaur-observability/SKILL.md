---
name: centaur-observability
description: "Investigate Centaur runtime behavior with the VictoriaLogs (`vlogs`) and VictoriaMetrics (`vmetrics`) Code Mode connectors. Use when a user reports something broken or stalled, a workflow, alert, or channel post never populated, asks to check code for issues, or needs thread, execution, sandbox, tool-call, service-health, or deploy evidence."
---

# Centaur Observability

Investigate runtime evidence before proposing redesigns, simplifications, or config rewiring. Skip this skill if the sandbox prompt says observability access is disabled.

## Workflow

1. Identify the affected surface: thread key, execution id, service, tool, and rough time window.
2. Read the relevant code paths and workflow status alongside the logs.
3. Query the narrowest matching evidence below in Code Mode (`centaur-codemode exec --result-only`). Run independent queries together in one cell.
4. Report a root cause when established, or bounded hypotheses plus the evidence needed to confirm them. Offer redesign advice only after findings, or when the user asks to skip investigation.

For auth, credential, or proxy failures, use the `auth-failure-log-triage` skill.

## Logs

Functions on `tools.vlogs`:

```js
await tools.vlogs.errors({ start: "1h" });
await tools.vlogs.errors({ service: "api", start: "6h" });
await tools.vlogs.thread_logs({ thread_key: "slack:T1:C1:1234", start: "24h" });
await tools.vlogs.thread_trace({ thread_key: "slack:T1:C1:1234" });
await tools.vlogs.execution_timeline({ execution_id: "exe_123" });
await tools.vlogs.slow_requests({ threshold_ms: 3000 });
await tools.vlogs.tool_calls({ tool_name: "websearch", start: "24h" });
await tools.vlogs.tool_analytics({ start: "7d" });
await tools.vlogs.service_health({ start: "1h" });
await tools.vlogs.sandbox_activity({ start: "1h" });
await tools.vlogs.query({ query: "level:error AND event:tool_call_completed", limit: 20 });
```

For example, one cell for a stalled thread:

```js
const thread_key = "slack:T1:C1:1234";
const [trace, errors, tools_used] = await Promise.allSettled([
  tools.vlogs.thread_trace({ thread_key, start: "24h" }),
  tools.vlogs.errors({ thread_key, start: "24h" }),
  tools.vlogs.tool_usage_by_thread({ thread_key, start: "24h" }),
]);
return { trace, errors, tools_used };
```

## Metrics

```js
const exprs = [
  "centaur_deployment_info",                 // component, version, git_sha, image
  "centaur_last_deploy_timestamp_seconds",   // last deploy per component
  "centaur_overlay_revision_info",           // deployed overlay revisions
  "centaur_overlay_revision_scrape_success", // overlay scrape health
];
return await Promise.all(exprs.map((expr) => tools.vmetrics.query({ expr })));
```

Run `centaur-codemode load vlogs` or `centaur-codemode load vmetrics` for every function and its arguments. If a query fails or returns nothing, say so; do not substitute repo files, values files, or memory for live state.
