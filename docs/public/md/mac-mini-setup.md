---
title: Running Centaur on a Mac Mini-style setup
description: Run Centaur on k3s with a Mac Mini, small VPS, or similar always-on host.
---

# Running Centaur on a Mac Mini-style setup

The easiest way to run Centaur outside a developer laptop is a small always-on
machine with k3s. This can be a Mac Mini running Linux, a DigitalOcean droplet,
another simple VPS, or a spare Linux box. You do not need a managed Kubernetes
cluster to get started.

Centaur publishes development images to GHCR. On a single small host, the
simplest setup is to point the local chart at those images instead of building
and importing images into k3s' container runtime.

## 1. Install k3s

Run these commands on the machine that will host Centaur:

```bash
curl -sfL https://get.k3s.io | sh -
sudo chmod 644 /etc/rancher/k3s/k3s.yaml
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
kubectl get nodes
```

Persist `KUBECONFIG` in your shell profile if you want future shells to target
this cluster automatically.

## 2. Install local tools

Install Docker plus the command-line tools Centaur's local workflow expects:

```bash
brew install just kubectl helm jq
```

If `brew` is not available on your Linux host, install Docker, `just`,
`kubectl`, `helm`, and `jq` from your package manager or their upstream
installers.

Clone Centaur on the host:

```bash
git clone <repo-url>
cd centaur
```

## 3. Use GHCR images

Use `source=ghcr` with the local Just recipes to point the chart at the
published `ghcr.io/paradigmxyz/centaur-*` images instead of local image names.
This keeps the chart's default `latest` tags and `IfNotPresent` pull policy
from `contrib/chart/values.dev.yaml`.

If GHCR access for the repository is private, create an image pull Secret and
add it to the chart with `global.imagePullSecrets`.

## 4. Bootstrap secrets

The default local chart expects one infra Secret named `centaur-infra-env`.
Export the required values before deploying:

```bash
export OP_SERVICE_ACCOUNT_TOKEN=...
export OP_VAULT=...
export SLACK_BOT_TOKEN=...
export SLACK_SIGNING_SECRET=...
export SLACKBOT_API_KEY=...
```

Then create the Kubernetes Secret:

```bash
just bootstrap-secrets
```

## 5. Deploy Centaur

Deploy the Helm chart with the GHCR image values:

```bash
just source=ghcr deploy
just status
```

Verify the API:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api -- \
  curl -fsS http://localhost:8000/health
```

Expected shape:

```json
{"status":"ok"}
```

Then continue with the [Quickstart](/quickstart) smoke test and agent-turn
verification steps.
