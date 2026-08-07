# Composio spike (Phase 1)

Question: can HubSpot + Microsoft 365 read/write paths run through the existing
[`tools/productivity/composio`](../../../tools/productivity/composio/) tool with
an acceptable auth and permission story?

## What Composio is here

- Sandbox CLI backed by Composio’s cloud API (`COMPOSIO_API_KEY`).
- Toolkit discovery and `tools.execute(...)` for many SaaS actions.
- Connected accounts are scoped by Composio `user_id` (default `"centaur"`).

## Findings vs Spaces kill-gate

| Requirement | Composio today | Verdict |
|-------------|----------------|---------|
| Per-principal secret isolation (Phase 0) | Default shared `user_id`; no iron-proxy grant path | Fail unless every principal maps to a distinct Composio user and connected account |
| Tokens stay in console / iron-proxy | Credentials live in Composio cloud | Diverges from Centaur OAuth + proxy model |
| Fail-closed egress allowlist | Outbound to Composio + whatever Composio reaches | Harder to reason about than direct HubSpot/Graph hosts |
| Portable Spaces tools | Client imports `centaur_sdk.secret` | Fine for Centaur tools; not a Spaces-native HTTP tool |

## Decision

**Defer native HubSpot/M365 CLIs only if** a follow-up spike proves:

1. Composio `user_id` is set to the Centaur principal id (or Teams/Slack user id) on every call.
2. Connected accounts are created via per-user OAuth and cannot be used by another principal.
3. Operators accept Composio as an additional trust/egress boundary.

Until then, prefer **console HubSpot + Microsoft OAuth strategies** (landed in
Phase 1) + iron-proxy grants, then thin HTTP tools under `tools/` that call
`api.hubapi.com` / `graph.microsoft.com` with placeholder credentials.

## Next check (when credentials exist)

```bash
# From a sandbox with COMPOSIO_API_KEY granted to one principal only:
composio health
# Then list HubSpot / Microsoft toolkits if present and attempt a read as
# principal A and a denied read as principal B.
```
