---
title: 🚧 Creating Apps
description: Work-in-progress design for deploying app-plane capabilities on Centaur.
---

# 🚧 Creating Apps

:::warning[🚧 Not implemented in production]
This is a design sketch based on
[paradigmxyz/centaur-old#759](https://github.com/paradigmxyz/centaur-old/pull/759).
The API names, manifest fields, rollout behavior, and security model may change
before this lands in the current Centaur repo.
:::

Apps are the proposed PaaS layer for Centaur. A team would ship a small repo or
container image, declare what it exposes, and let Centaur deploy it next to the
agent control plane. The app can contribute tools, skills, workflows, personas,
and a web surface without forking the base Centaur repo.

The useful split is:

- **Core repo**: stable runtime, API auth, sandboxes, workflow primitives,
  tool routing, Helm chart, and shared security boundaries.
- **Org overlay repo**: reviewed static tools, workflows, skills, personas, and
  defaults for one installation.
- **App repos**: independently released capabilities and web apps that Centaur
  can register, deploy, proxy, and remove.

## What an app contains

An app release would be a versioned record with:

- A deployable source, either an image or a Git repo plus ref and commit SHA.
- A `centaur.app.toml` manifest, or equivalent JSON posted to the API.
- One web process listening on a declared port.
- Optional capability declarations for tools, skills, workflows, personas, and
  web routes.

```toml
[app]
name = "research-tool"
repo_url = "https://github.com/example/research-tool"
ref = "main"
commit_sha = "abc123"
image = "ghcr.io/example/research-tool:sha-abc123"
port = 8080

[web]
enabled = true

[[tools]]
name = "research-tool"
description = "Search private research data"
methods = [
  { name = "search", path = "/tools/research-tool/search" },
]

[[skills]]
name = "research-skill"
description = "How to use the research corpus"

[[workflows]]
name = "research-digest"
description = "Generate a research digest"

[[personas]]
name = "researcher"
description = "Research-oriented agent defaults"
```

If an app ships source instead of an image, the draft reconciler can clone the
repo and run a configured `build_cmd` and `start_cmd`. The old design also
included simple auto-detection for Node, Next.js, and Python projects, with an
explicit `start_cmd` required when no supported entrypoint is found.

## Lifecycle

The proposed app lifecycle is:

1. CI builds an app image, or publishes a repo commit that Centaur can clone.
2. CI calls `POST /apps` with the app name, source, version, port, and manifest.
3. The API stores app desired state in Postgres.
4. A reconciler creates or updates one Kubernetes Deployment, Service, and
   NetworkPolicy for the active app release.
5. The API proxies web and capability requests through the existing control
   plane.
6. Operators can list apps, inspect logs, restart, roll forward, or delete the
   app through lifecycle endpoints.

In the old design, app state lived in `apps`, `app_releases`,
`app_capabilities`, and `app_deployments`. Releases could move through pending,
deploying, active, failed, deleting, and deleted states.

## Routing model

The app plane keeps the API as the registry, auth boundary, and router:

| Surface | Proposed route |
|---------|----------------|
| Web app | `/apps/{name}/...` |
| App metadata | `GET /apps/{name}` |
| Logs | `GET /apps/{name}/logs` |
| Restart | `POST /apps/{name}/restart` |
| Delete | `DELETE /apps/{name}` |
| Tool method | Existing `/tools/{tool}/{method}` route proxies to the app |
| Skills | Listed through app skill discovery and fetched lazily |
| Workflows | Started through the existing workflow run API |

That lets a Slack workflow, API client, web dashboard, or agent call the same
capability without knowing whether it came from core Centaur, an overlay, or an
app release.

## Security shape

The old PR kept the app runtime narrow:

- App pods run without Kubernetes service account tokens.
- Containers run with `allowPrivilegeEscalation: false`, dropped Linux
  capabilities, and a runtime seccomp profile.
- NetworkPolicy allows ingress only from the API to the app port.
- Egress is limited to DNS and the API, with temporary HTTPS egress only when a
  source clone is needed.
- The API strips sensitive inbound headers before proxying to an app, then adds
  app identity headers such as `x-centaur-app`.
- App-scoped API keys can be limited to broad app access or to one app.

The production version should keep secrets flowing through the same
credential-safe boundary as the rest of Centaur: apps should receive placeholders
or scoped runtime credentials, not long-lived organization secrets by default.

## Why this matters

This is where Centaur starts to feel like a PaaS for agent infrastructure.
Instead of asking teams to fork Centaur for every custom surface, they can ship
small app repos that plug into the shared control plane:

- A department dashboard can expose a web UI and typed tools.
- A data team can deploy a workflow and companion skill in one release.
- A platform team can publish a persona plus approved tools behind the same
  policy boundary.
- An app can be upgraded, rolled back, or deleted without changing the base
  Centaur chart.

## Open design work

Before this becomes production documentation, the current repo still needs the
implementation and a few product decisions:

- Final manifest schema and compatibility guarantees.
- Build provenance, image trust, and source clone policy.
- Per-app domains, auth, and public/private routing.
- Secrets and environment-variable handoff for app runtimes.
- Rollout strategy, health checks, and failure recovery.
- Observability shape for app logs, metrics, traces, and audit events.
