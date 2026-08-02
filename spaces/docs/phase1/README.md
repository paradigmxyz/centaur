# Phase 1 — Identities and tools

| Doc | Purpose |
|-----|---------|
| [`composio-spike.md`](composio-spike.md) | Evaluate Composio vs native HubSpot/M365 tools |
| [`teamsbot-e2e.md`](teamsbot-e2e.md) | Checklist for two-user Teams identity proof |

## Landed in-repo

- `spaces/adapter/` — thin Centaur session HTTP client
- `spaces/schema/` — placeholder for Phase 2 durable product state
- CI: `.github/scripts/check-spaces-import-boundary.sh`
- Console OAuth providers: `hubspot`, `microsoft` (+ HubSpot identity enrichment job)

## Still blocked on always-on host + real apps

- Public HTTPS for console OAuth callbacks and Teams `/api/messages`
- HubSpot developer app + Entra app client secrets (lock iron-proxy domain allowlist first)
- Teamsbot enablement and two-staff consent proof
