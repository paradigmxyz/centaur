# Escape hatch — what a Centaur replacement must provide

Standing test: if Centaur were replaced tomorrow, rewrite only the adapter (+ any OAuth strategies not upstreamed).

## Required capabilities

1. **Durable sessions** — create/reuse by stable thread key; append messages; start execution; replay events (or equivalent transcript + completion stream).
2. **Per-identity principals** — map platform user/tenant to an opaque principal id; grant secrets/roles per principal; deny cross-principal secret resolution.
3. **Credential injection without sandbox exposure** — placeholders in the agent environment; outbound substitution on allowlisted hosts only.
4. **Isolated execution** — one workload per turn/session with filesystem + network isolation from other tenants.
5. **Model harness selection** — at least one chat/completions-capable harness (Claude or OpenAI) selectable per turn.
6. **Auditability** — durable record of who ran what; Spaces will *also* write its own audit at decision points (opaque IDs only).

## Explicitly not required from the replacement

- Slack/Teams ingress (we can reattach adapters)
- Console UI (operator tooling can be Spaces-owned later)
- SharePoint / HubSpot business logic (ours)

## Current adapter surface (Centaur)

| Concern | Centaur surface |
|---------|-----------------|
| Session | `POST/GET /api/session/{thread}` , `/messages`, `/execute`, `/events` |
| Principal | Console `/api/v1/principals`, grants, `effective_config`; session field `iron_control_principal` |
| Secrets | Static secrets + grants; iron-proxy sync |
| Platform bind | Thread key + metadata (`slack_user_id`, Teams actor ids, …) |

## Metrics to track

- Adapter LOC
- Time-to-merge on upstream Centaur sync
- Import-boundary violations (`spaces/` must not import Centaur packages)
