# Phase 1 — Identities and tools

| Doc | Purpose |
|-----|---------|
| [`composio-spike.md`](composio-spike.md) | Evaluate Composio vs native HubSpot/M365 tools |
| [`teamsbot-e2e.md`](teamsbot-e2e.md) | Checklist for two-user Teams identity proof |
| [`oauth-app-registration.md`](oauth-app-registration.md) | HubSpot and Entra OAuth registration checklist |
| [`droplet-bootstrap.sh`](droplet-bootstrap.sh) | Ubuntu LTS host bootstrap for the migration |
| [`ci-status.md`](ci-status.md) | Fork PR Actions approval note |

## Landed in-repo

- `spaces/adapter/` — thin Centaur session HTTP client
- `spaces/schema/` — placeholder for Phase 2 durable product state
- CI: `.github/scripts/check-spaces-import-boundary.sh`
- Console OAuth providers: `hubspot`, `microsoft` (+ HubSpot identity enrichment job)
- Managed egress lock: `ironProxy.allowlistDomains` + [`values.spaces.yaml`](../../../contrib/chart/values.spaces.yaml)
- Tools: `tools/business/hubspot`, `tools/business/microsoft_graph` (need real tokens for live calls)

## Still blocked on always-on host + real apps

- Public HTTPS for console OAuth callbacks and Teams `/api/messages`
- HubSpot developer app + Entra app client secrets (after allowlist overlay is deployed)
- Teamsbot enablement and two-staff consent proof
