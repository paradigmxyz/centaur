---
name: auth-failure-log-triage
description: "Investigate reported auth, credential, permission, API proxy, 401, 403, 503, OAuth, token, or secret-resolution failures by querying VictoriaLogs first. Use when a user says an integration auth failed, a tool got unauthorized/forbidden/service unavailable, an api-proxy request failed, or asks why credentials/secrets are not working."
---

# Auth Failure Log Triage

When a user reports an auth, permission, credential, API proxy, OAuth, token, secret, `401`, `403`, or `503` failure, inspect live runtime evidence before proposing secret or permission changes.

## Workflow

1. Confirm the affected surface from the user message: tool/integration name, URL host, thread key, execution id, pod/service, and rough time window.
2. Discover `vlogs` if needed:

```bash
call discover vlogs
```

3. Query likely auth failures in the last 24h, narrowing when the user gave a host, pod, thread, or execution id.

For Centaur API proxy / Iron Proxy issues:

```bash
call vlogs query '{"query":"kubernetes.pod_name:centaur-api-proxy-* AND (audit.status_code:401 OR audit.status_code:403 OR audit.status_code:503 OR \"failed to fetch secret\" OR unauthorized OR forbidden OR auth OR oauth OR token OR secret)","start":"24h","limit":100}'
```

For a specific thread:

```bash
call vlogs thread_trace '{"thread_key":"THREAD_KEY","start":"24h","limit":500}'
call vlogs tool_usage_by_thread '{"thread_key":"THREAD_KEY","start":"24h","limit":200}'
```

For a specific execution:

```bash
call vlogs execution_timeline '{"execution_id":"EXECUTION_ID"}'
```

4. If the direct query is empty, find labels first:

```bash
call vlogs field_values '{"field":"service","query":"*proxy*","start":"24h","limit":200}'
call vlogs field_names '{"query":"service:iron-proxy","start":"24h"}'
call vlogs streams '{"query":"proxy","start":"24h"}'
```

5. Summarize only what the logs support:

- Counts by status code: `401`, `403`, `503`.
- Secret-resolution errors and the secret reference name, without exposing secret values.
- Timestamp, pod, service, host, method, path, and status for concrete failures.
- Whether auth transforms injected credentials, if shown by proxy fields such as `request_transforms`.
- Distinguish upstream denial (`audit.status_code=403`) from local proxy/secret resolution failure (`failed to fetch secret`).

## Output Rules

- Lead with whether log evidence was found.
- Do not claim a secret or permission is wrong unless logs show that.
- Do not recommend secret rewiring until you have checked live `vlogs` evidence and compared the relevant error shape.
- If no logs are found, say which query/window was checked and ask for the missing discriminator: time, thread key, execution id, host, or integration name.
