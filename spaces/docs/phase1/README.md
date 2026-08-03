# Phase 1 — Identities and tools

| Doc | Purpose |
|-----|---------|
| [`composio-spike.md`](composio-spike.md) | Evaluate Composio vs native HubSpot/M365 tools |
| [`teamsbot-e2e.md`](teamsbot-e2e.md) | Checklist for two-user Teams identity proof |
| [`oauth-app-registration.md`](oauth-app-registration.md) | HubSpot and Entra OAuth registration checklist |
| [`droplet-bootstrap.sh`](droplet-bootstrap.sh) | Ubuntu LTS host bootstrap for the migration |
| [`vps-status.md`](vps-status.md) | Live CubePath host status + Cloudflare Tunnel tomorrow checklist |
| [`ci-status.md`](ci-status.md) | Fork-only validation policy |

## Landed in-repo

- `spaces/adapter/` — thin Centaur session HTTP client
- `spaces/schema/` — placeholder for Phase 2 durable product state
- CI: `.github/scripts/check-spaces-import-boundary.sh`
- Console OAuth providers: `hubspot`, `microsoft` (+ HubSpot identity enrichment job)
- Managed egress lock: `ironProxy.allowlistDomains` + [`values.spaces.yaml`](../../../contrib/chart/values.spaces.yaml)
- Tools: `tools/business/hubspot`, `tools/business/microsoft_graph` (need real tokens for live calls)

## Host status

CubePath solo VPS is up; in-cluster Claude smoke passed. See [`vps-status.md`](vps-status.md).

## Still blocked on public HTTPS + real apps

- Cloudflare Tunnel (or Funnel) for console OAuth callbacks and Teams `/api/messages`
- HubSpot developer app + Entra app client secrets (after tunnel URLs exist)
- Teamsbot enablement and two-staff consent proof
