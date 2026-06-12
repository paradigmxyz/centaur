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

If you are evaluating on macOS, especially Apple Silicon, use the local-build
path below. Native k3s is Linux-only, and published images are currently x86 only. In that case, build the images locally and load them into the local cluster runtime.

## macOS local evaluation with kind

This is the quickest reproducible laptop path when GHCR images do not match
your Mac's architecture.

```bash
brew install just kubectl helm jq kind cloudflared
kind create cluster --name centaur
kubectl config use-context kind-centaur
```

Export the same bootstrap secrets as the Linux path below, then build and load
local images:

```bash
just build
kind load docker-image \
  centaur-api-rs:latest \
  centaur-slackbotv2:latest \
  centaur-iron-proxy:latest \
  centaur-agent:latest \
  --name centaur
```

Kind nodes have their own containerd image store, so local `docker build` images
are not visible to Kubernetes until you run `kind load docker-image`. The same
separate-image-store rule applies to k3s; use `just up k3s` there to import
images into k3s containerd.

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
kubectl exec -n centaur deploy/centaur-centaur-api-rs -- \
  curl -fsS http://localhost:8080/healthz
```

Expected shape:

```json
{"status":"ok"}
```

Then continue with the [Quickstart](/quickstart) smoke test and agent-turn
verification steps.

## 6. Optional: expose local Slackbot with a tunnel

If you are running Centaur only on your laptop, Slack cannot reach the in-cluster
Slackbot service directly. Use any HTTPS tunnel that can forward to localhost,
such as Cloudflare Tunnel, ngrok, zrok, or Tailscale Funnel. For example, with
Cloudflare Tunnel, forward Slackbot to localhost and expose it with a temporary
HTTPS URL:

```bash
kubectl port-forward -n centaur svc/centaur-centaur-slackbotv2 3001:3001
```

In another terminal:

```bash
cloudflared tunnel --url http://localhost:3001
```

Use the generated `https://*.trycloudflare.com` URL as the host in the
[Quickstart Slack webhook setup](/quickstart#61-set-up-the-slack-app):

```text
https://<trycloudflare-host>/api/webhooks/slack
```

Temporary tunnel URLs usually change when the tunnel restarts, so update the
Slack Request URL each time or configure a named tunnel/domain.

For a durable, in-cluster alternative that keeps a stable public URL, see
[Expose the Slackbot with Tailscale Funnel](/operate/tailscale-funnel).
