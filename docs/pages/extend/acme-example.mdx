---
title: ACME example
description: Use the centaur-acme overlay and centaur-acme-infra GitOps template as a forkable starting point for your own Centaur deployment.
---

# ACME example

The fastest way to understand a real Centaur deployment is to start from the
ACME example repos:

- [`paradigmxyz/centaur-acme`](https://github.com/paradigmxyz/centaur-acme) is a
  small organization overlay. Fork it when you want to add your own tools,
  workflows, skills, personas, or sandbox prompt guidance.
- [`paradigmxyz/centaur-acme-infra`](https://github.com/paradigmxyz/centaur-acme-infra)
  is a GitOps deployment template. Fork it when you want an Argo CD-managed
  cluster layout that installs Centaur and mounts the ACME overlay image.

Together they show the recommended split: keep reusable Centaur in this repo,
keep organization-specific agent behavior in an overlay repo, and keep cluster
configuration in an infra repo.

## Repository roles

| Repository | Purpose | Contains |
|------------|---------|----------|
| `centaur` | Base platform | Helm chart, API, sandbox image, Slackbot, SDK, built-in tools and workflows. |
| `centaur-acme` | Example organization overlay | `tools/acme_crm`, `workflows/daily_acme_brief.py`, `.agents/skills/acme-support`, and `services/sandbox/SYSTEM_PROMPT.md`. |
| `centaur-acme-infra` | Example deployment repo | Argo CD bootstrap app, Centaur Helm values, and optional raw manifests managed with the app. |

Use `centaur-acme` to learn how to package what your agents know and can call.
Use `centaur-acme-infra` to learn how that package is mounted into a running
Centaur deployment.

## 1. Fork the example repos

Create your own overlay and infra repos:

```bash
gh repo fork paradigmxyz/centaur-acme --clone
gh repo fork paradigmxyz/centaur-acme-infra --clone
```

Replace the ACME names after forking. Most teams keep the same split:

```text
your-org/
├── centaur-overlay       # forked from centaur-acme
└── centaur-infra         # forked from centaur-acme-infra
```

## 2. Customize the overlay

In the overlay repo, keep only the extension points you need:

```text
centaur-acme/
├── Dockerfile
├── tools/
│   └── acme_crm/
├── workflows/
│   └── daily_acme_brief.py
├── .agents/
│   └── skills/
│       └── acme-support/
└── services/
    └── sandbox/
        └── SYSTEM_PROMPT.md
```

The included `tools/acme_crm` tool is intentionally toy-sized and credential
free. Use it as a shape reference for a real internal tool: a `client.py`, a
`pyproject.toml`, and optionally a thin `cli.py` for local testing.

The included workflow demonstrates how an overlay can add durable workflows
without changing the base Centaur API. The included skill and sandbox prompt
show how to package organization-specific agent guidance.

## 3. Build and publish the overlay image

The overlay `Dockerfile` copies the repo to `/overlay`:

```dockerfile
FROM alpine:3.20

WORKDIR /overlay
COPY . /overlay
```

Build and publish an image for your fork:

```bash
docker build -t ghcr.io/your-org/centaur-overlay:sha-$(git rev-parse --short HEAD) .
docker push ghcr.io/your-org/centaur-overlay:sha-$(git rev-parse --short HEAD)
```

The Centaur chart mounts `sourcePath: /overlay` into the API at
`/app/overlay/org` and into sandbox pods at `/home/agent/overlay/org`.

## 4. Point the infra repo at your images

In the infra repo, update
`clusters/acme-centaur/argocd/bootstrap/centaur.yaml`.

Set the overlay image repository and tag:

```yaml
parameters:
  - name: overlay.image.repository
    value: ghcr.io/your-org/centaur-overlay
    forceString: true
  - name: overlay.image.tag
    value: sha-0000000
    forceString: true
```

The template also pins the base Centaur service images:

```yaml
- name: api.image.tag
  value: sha-0000000
- name: slackbot.image.tag
  value: sha-0000000
- name: sandbox.image.tag
  value: sha-0000000
- name: ironProxy.image.tag
  value: sha-0000000
```

Replace those tags with images you built from `centaur`, or wire them to your
image automation. The example includes Argo CD Image Updater annotations for
the overlay image.

For production, pin the Centaur chart source to a commit SHA instead of tracking
`main`:

```yaml
sources:
  - repoURL: https://github.com/paradigmxyz/centaur.git
    targetRevision: <commit-sha>
    path: contrib/chart
```

## 5. Configure Helm values and secrets

The example values live at
`clusters/acme-centaur/argocd/values/centaur.yaml`.

Before applying the app, create the Centaur infra Secret in the target
namespace. The local quickstart documents the same keys, and production
deployments usually provide them through your secret manager or GitOps secret
workflow:

```bash
kubectl create namespace centaur-system
kubectl create secret generic centaur-infra-env \
  --namespace centaur-system \
  --from-literal=OP_SERVICE_ACCOUNT_TOKEN=... \
  --from-literal=OP_VAULT=... \
  --from-literal=SLACK_BOT_TOKEN=... \
  --from-literal=SLACK_SIGNING_SECRET=... \
  --from-literal=SLACKBOT_API_KEY=...
```

Model and tool credentials such as `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`,
`AMP_API_KEY`, and `GITHUB_TOKEN` should be configured through Centaur's
credential source. Sandboxes should receive placeholders; iron-proxy injects the
real values only for approved outbound requests.

## 6. Bootstrap Argo CD

After Argo CD is installed in the cluster, apply the bootstrap manifests from
the infra repo:

```bash
kubectl apply -f clusters/acme-centaur/argocd/bootstrap/00-namespaces.yaml
kubectl apply -f clusters/acme-centaur/argocd/bootstrap/centaur.yaml
```

Argo CD installs the Centaur Helm chart, applies the values from the infra repo,
and mounts the overlay image from your overlay repo.

## 7. Verify the running overlay

From the API pod, verify API-side discovery:

```bash
kubectl exec -n centaur-system deploy/centaur-centaur-api -- \
  sh -lc 'echo "$TOOL_DIRS"; echo "$WORKFLOW_DIRS"; ls -la /app/overlay/org'
```

Expected paths include:

```text
/app/overlay/org/tools
/app/overlay/org/workflows
```

From a sandbox, verify sandbox-side guidance:

```bash
echo "$CENTAUR_OVERLAY_DIR"
ls "$CENTAUR_OVERLAY_DIR"
ls "$CENTAUR_OVERLAY_DIR/.agents/skills"
```

Expected paths include:

```text
/home/agent/overlay/org/.agents/skills
/home/agent/overlay/org/services/sandbox/SYSTEM_PROMPT.md
```

You can also inspect the runtime payload for a thread:

```bash
curl -s "$CENTAUR_API_URL/agent/runtime?key=$THREAD_KEY" \
  -H "X-Api-Key: $CENTAUR_API_KEY" | jq '.overlay'
```

## What to change first

Start small:

1. Rename `tools/acme_crm` to one internal tool your agents should be able to
   call.
2. Replace `.agents/skills/acme-support/SKILL.md` with one real playbook your
   team already follows.
3. Add your organization's sandbox prompt guidance to
   `services/sandbox/SYSTEM_PROMPT.md`.
4. Publish the overlay image and update the infra repo's `overlay.image.*`
   values.
5. Verify discovery from the API pod and from a sandbox before adding more
   tools or workflows.

Once that path works, extend the overlay incrementally. The goal is to keep the
base `centaur` repo boring and reusable while making your overlay the home for
everything specific to your organization.
