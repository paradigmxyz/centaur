---
title: Quickstart
description: Boot Centaur locally and verify the control plane.
---

# Quickstart

This guide gets you from a fresh checkout to a working local Centaur stack. You
do not need a full production Kubernetes installation for local setup: a
lightweight k3s-based cluster is enough. The happy path is: point `kubectl` at
that cluster, bootstrap the required infra Secret, run `just up`, verify the
API, then run one agent turn without Slack. If you want the easiest small-host
path first, start with [Running Centaur on a Mac Mini-style
setup](/mac-mini-setup).

If you want an agent to drive setup with you, point it at these docs: every page
is available through `/llms.txt`, `/llms-full.txt`, and static Markdown files
such as `/md/quickstart.md`.

## 1. Install prerequisites

From the repo root:

```bash
brew install just kubectl helm jq
```

You also need Docker and a local Kubernetes cluster. This can be lightweight:
k3s works on a small VPS, DigitalOcean droplet, Linux box, or Mac Mini-style
host. Docker Desktop with Kubernetes enabled, kind, and minikube are also fine
as long as `kubectl` points at that local cluster and it can run the Helm chart.
The [Mac Mini-style setup guide](/mac-mini-setup) walks through the k3s path.

Check the target before booting Centaur:

```bash
kubectl config current-context
kubectl get nodes
```

The `Justfile` builds local images named `centaur-api:latest`,
`centaur-iron-proxy:latest`, `centaur-slackbot:latest`, and
`centaur-agent:latest`, then deploys `contrib/chart` with
`contrib/chart/values.dev.yaml`.

## 2. Export bootstrap secrets

The default local chart expects one infra Secret named `centaur-infra-env`.
`just bootstrap-secrets` creates it from your shell environment.

`just bootstrap-secrets` currently requires these shell variables:

```bash
export OP_SERVICE_ACCOUNT_TOKEN=...
export OP_VAULT=...
export SLACK_BOT_TOKEN=...
export SLACK_SIGNING_SECRET=...
export SLACKBOT_API_KEY=...
```

Create the Slackbot app at [api.slack.com/apps](https://api.slack.com/apps).
Use the app's Bot User OAuth Token for `SLACK_BOT_TOKEN` and its Signing Secret
for `SLACK_SIGNING_SECRET`.

`OP_SERVICE_ACCOUNT_TOKEN` and `OP_VAULT` let [iron-proxy](https://docs.iron.sh)
resolve model and tool credentials through 1Password. `SLACK_SIGNING_SECRET`
and `SLACKBOT_API_KEY` are API boot requirements in the current chart.
`SLACK_BOT_TOKEN` is required by the default local bootstrap because Slackbot is
enabled in `values.dev.yaml`; use a real token if you want to test Slack.

`SLACKBOT_API_KEY` is a static service token. The API bootstraps that value into
Postgres on startup, so it must exist before `just up`.

Application-level model and tool secrets, such as `OPENAI_API_KEY`,
`ANTHROPIC_API_KEY`, `AMP_API_KEY`, and `GITHUB_TOKEN`, should live in
1Password or the configured [iron-proxy](https://docs.iron.sh) secret source. Sandboxes receive
placeholder values and [iron-proxy](https://docs.iron.sh) injects the real credentials only on approved
outbound requests.

The default harness is `codex`, so `OPENAI_API_KEY` must exist in the configured
secret source before Slack agent turns can complete. Use explicit harness
selectors only when you want a non-default harness such as Amp or Claude Code.

## 3. Boot the stack

```bash
just up
```

That runs:

1. `just bootstrap-secrets`
2. `just build`
3. `just deploy`

Check the namespace:

```bash
just status
```

## 4. Verify the API

The API exposes localhost inside its own deployment. Localhost bypasses external
API-key auth, which is why the health check runs through `kubectl exec`:

```bash
kubectl exec -n centaur deploy/centaur-centaur-api -- \
  curl -fsS http://localhost:8000/health
```

Expected shape:

```json
{"status":"ok"}
```

## 5. Run an agent turn

Before testing Slack, run the local smoke test. It uses the same durable agent
API that Slackbot uses: spawn or reuse a runtime, persist a message, enqueue an
execution, and poll the execution state until the result contains `PONG`.

```bash
just smoke
```

The successful result includes the terminal execution row. The important fields
are:

```json
{
  "status": "completed",
  "result_text": "...PONG..."
}
```

If the smoke test times out or fails, start with the local stack state:

```bash
just status
just logs api
kubectl get pods -n centaur -l centaur.ai/managed=true
```

If you changed the namespace or release name, set `CENTAUR_NAMESPACE` and
`CENTAUR_RELEASE` before running `just smoke` so the recipe targets the right
deployment.

## 6. Try Slack after the API works

Mention the bot in a test channel where the Slack app is installed:

```text
@<your bot's username> reply with exactly PONG
```

Slack messages without a harness flag use Codex. Add a selector such as
`--amp`, `--claude`, or `--pi` only when you want to override the default.

If Slack receives the mention but no agent runs, inspect Slackbot logs:

```bash
just logs slackbot
```
