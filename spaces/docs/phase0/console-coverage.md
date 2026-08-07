# Console coverage vs Spaces needs

Assessed against the running local console (Rails) and API we exercised in Phase 0.

## Covered well enough (no custom web UI required for Phase 1)

| Need | Evidence |
|------|----------|
| Principals | API upsert/lookup; UI routes under `/principals` |
| Roles / grants | API create/list; UI for roles and grants |
| Static secrets | API + UI (`/secrets/static`) |
| Effective config | `GET …/effective_config` (used for kill-gate) |
| OAuth Apps | UI + API; provider strategies in registry |
| Broker credentials | Models/UI for consent-linked credentials |
| Password login | Enabled in chart for local (`CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED`) |
| Threads observer | Console threads surface is read-oriented (not a second control plane) |

## Gaps / watch items

| Gap | Impact | Decision |
|-----|--------|----------|
| No HubSpot / Microsoft OAuth strategies | Blocks per-user CRM/Graph consent | Phase 1 build (strategy + registry) |
| HostAuthorization blocks `Host: localhost` inside pod | Local curl-from-console-pod fails | Call via in-cluster Service DNS (documented) |
| Bootstrap API key must match seeded admin | Secret CR strip broke auth until postgres recreate | Fixed; keep Windows CR hygiene |
| Shared infra secrets appear on principals after session register | Dilutes “exactly one secret” counts | Isolation still holds for *our* grants; watch default role grants |

## Verdict

**Do not build a custom Spaces web console for Phase 1.** Use Centaur console + `centaur-perms` / API scripts. Revisit only if Sales Scout needs operator UX the console cannot provide (e.g. space budgets, SharePoint curator flows) — that belongs in Spaces UI later, not a fork of iron-control.
