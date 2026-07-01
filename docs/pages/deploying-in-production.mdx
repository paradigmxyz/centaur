---
title: Deploying in Production
description: Configure secrets, Slack, harness credentials, Kubernetes sandboxes, and production verification for Centaur.
---

# Deploying in Production

Production Centaur is a Kubernetes deployment with durable API state in
Postgres, sandbox pods for agent execution, and [iron-proxy](https://docs.iron.sh) for credential
injection. The goal is a small working deployment with a clear operator before
you add more tools, workflows, harnesses, or overlays.

## Production shape

The API saves threads, runs, and events in Postgres. The Kubernetes backend
creates sandbox pods for agent work. [iron-proxy](https://docs.iron.sh) handles outbound requests that
need credentials:

<figure className="architecture-figure">
  <img src="/brand/workflow.svg" alt="Centaur production workflow — Centaur API plus Postgres hands a run to the Kubernetes backend, which attaches a sandbox pod whose outbound HTTP routes through iron-proxy" />
  <figcaption>Slackbot and API ingress → Centaur API (Postgres-backed) → Kubernetes sandbox runtime → outbound traffic through iron-proxy.</figcaption>
</figure>

Each pod receives the prompt files, environment, proxy CA, proxy settings, and
command it needs for one assigned thread. It should not receive raw model keys
or third-party API keys.

## 1. Choose the operating boundary

Before installing, decide:

| Question | Why it matters |
|----------|----------------|
| Who is the operator? | Someone must own secrets, upgrades, incidents, and access reviews. |
| What Slack workspace and channels matter? | Defines the first user and permission boundary. |
| What repos should agents work on? | Determines GitHub token scope and repo cache needs. |
| What tools or data sources matter first? | Keeps setup focused on one useful loop. |
| What is sensitive? | Determines private channels, tool scopes, and review requirements. |

Good first deployments have one narrow engineering, research, support, security,
data, or operations workflow where agents can call real tools.

## 2. Create the infra secret

The Helm chart reads infrastructure values from an existing Kubernetes Secret.
By default that Secret is named `centaur-infra-env`:

```yaml
secretManager:
  existingSecretName: centaur-infra-env
  envPrefix: ""
```

For local development, `just bootstrap-secrets` creates this Secret from your
shell environment. In production, create it through your normal secret delivery
path before installing the chart.

Minimum keys:

| Secret | Required for | Notes |
|--------|--------------|-------|
| `DATABASE_URL` | API | Postgres connection string. |
| `IRON_MANAGEMENT_API_KEY` | [iron-proxy](https://docs.iron.sh) management API | Generate with `openssl rand -hex 32`. |
| `SANDBOX_SIGNING_KEY` | Sandbox API tokens | Generate with `openssl rand -hex 32`; keeps sandbox tokens valid across API restarts. |
| `SLACK_BOT_TOKEN` | Slackbot | Bot User OAuth Token from the Slack app. |
| `SLACK_SIGNING_SECRET` | Slackbot/API | Used to verify Slack webhook signatures. |
| `SLACKBOT_API_KEY` | Slackbot to API | Static service token; API bootstraps it into Postgres on startup with `agent` scope. |
| `OP_CONNECT_TOKEN` | [iron-proxy](https://docs.iron.sh) 1Password Connect source (preferred) | Needed when `ironProxy.secretSource` is `onepassword-connect`. |
| `OP_SERVICE_ACCOUNT_TOKEN` | [iron-proxy](https://docs.iron.sh) 1Password service-account source | Needed when `ironProxy.secretSource` is `onepassword`. |
| `OP_VAULT` | [iron-proxy](https://docs.iron.sh) 1Password source | Vault name or id used for `op://` references (either mode). |

`SLACKBOT_API_KEY` is not created with the admin API during initial boot, because
the API process requires it before it can start. Generate a high-entropy value,
store it in the infra Secret, and reuse the same value in Slackbot.

## 3. Configure harness credentials

Store one secret per enabled harness credential:

| Harness | API value | Slack selector | Credential to store | Upstream |
|---------|-----------|----------------|---------------------|----------|
| Codex default | `codex` | none or `--codex` | `OPENAI_API_KEY` | `api.openai.com` |
| Codex with OpenRouter provider | `codex` | none or `--codex` | `OPENROUTER_API_KEY` | `openrouter.ai` |
| Amp | `amp` | `--amp` | `AMP_API_KEY` | `ampcode.com` |
| Claude Code | `claude-code` | `--claude` | `ANTHROPIC_API_KEY` | `api.anthropic.com` |
| pi-mono | `pi-mono` | `--pi` | `ANTHROPIC_API_KEY` | `api.anthropic.com` |

In normal sandbox mode, containers receive placeholder values such as
`OPENAI_API_KEY=OPENAI_API_KEY`. [iron-proxy](https://docs.iron.sh) swaps the
placeholder for the real key on outbound requests, only on the hosts and
headers the secret is bound to.

When `ironProxy.secretSource` is `onepassword`, [iron-proxy](https://docs.iron.sh) resolves these values
from `op://$OP_VAULT/<SECRET_NAME>/credential`. For example, store the default
Codex credential in a 1Password item named `OPENAI_API_KEY`. To run Codex
through OpenRouter, store `OPENROUTER_API_KEY` and set `OPENROUTER_MODEL` to a
model slug such as `openrouter/auto`, or set `CODEX_MODEL_PROVIDER=openrouter`
alongside `CODEX_MODEL`. Per-turn Codex model overrides with provider-style
slugs such as `--model anthropic/claude-fable-5` also select the OpenRouter
provider even when `OPENROUTER_MODEL` is unset.

Whatever source you pick, the vault is shared across the whole deployment,
so any thread can use any configured credential. Per-user and per-channel
scoping is on the roadmap; until then, scope tool and harness access
accordingly. See [Security](/security) for the full threat model.

### Codex Auth Modes

:::warning[Dedicate the account to Centaur]
Do not use this ChatGPT account for `codex` outside Centaur once its
refresh token is in the broker. OpenAI's OAuth flow uses strict refresh
token reuse detection: if you keep running `codex` locally with the same
account, both clients will race to rotate the refresh token. Whichever
side rotates second is treated as a stolen credential and the entire
token family is revoked, logging both sides out at random. Use a separate
ChatGPT account for any non-Centaur Codex work.
:::

Codex supports two authentication modes, selected per deployment with the
`CODEX_AUTH_MODE` env var on the sandbox (set it via `sandbox.extraEnv`):

| Mode | Upstream | Secrets required |
|------|----------|------------------|
| `api_key` (default) | `api.openai.com` | `OPENAI_API_KEY` |
| `access_token` | `chatgpt.com` | `OPENAI_CODEX_CLIENT_ID`, `OPENAI_CODEX_BLOB`, `OPENAI_CODEX_ACCOUNT_ID` |

`access_token` mode routes Codex through a ChatGPT account rather than a raw
API key. [iron-token-broker](https://docs.iron.sh) holds the refresh token
and mints short-lived access tokens, which iron-proxy injects on outbound
requests so the sandbox never sees them.

Store these three items in your secrets backend (1Password vault, Kubernetes
Secret, etc.) when running in `access_token` mode:

- `OPENAI_CODEX_CLIENT_ID`: the Codex CLI's OAuth client id. This is a
  fixed, publicly known constant: `app_EMoamEEZ73f0CkXaXp7hrann`. It is
  the same for every Codex install and never rotates, but the broker
  still resolves it through your secrets backend, so store the literal
  value as-is.
- `OPENAI_CODEX_BLOB`: a JSON document `{"refresh_token": "..."}`. The
  broker rotates this in place on every refresh, so the backing item must
  be writable.
- `OPENAI_CODEX_ACCOUNT_ID`: the ChatGPT account UUID the credential is
  bound to. It is static, but iron-proxy injects it as the
  `chatgpt-account-id` header so the backend can route to the right
  workspace. Store it alongside the other two, not in code.

To bootstrap, run `codex login` locally, then copy the refresh token and
account id from `~/.codex/auth.json` into the matching secret items. Use
the constant above for `OPENAI_CODEX_CLIENT_ID`.

### Claude Auth Modes

:::warning[Dedicate the account to Centaur]
Do not use this Claude.ai account for `claude` outside Centaur once its
refresh token is in the broker. Anthropic's OAuth flow uses strict
refresh token reuse detection: if you keep running `claude` locally with
the same account, both clients will race to rotate the refresh token.
Whichever side rotates second is treated as a stolen credential and the
entire token family is revoked, logging both sides out at random. Use a
separate Claude.ai account for any non-Centaur Claude Code work.
:::

Claude Code supports two authentication modes, selected per deployment
with the `CLAUDE_CODE_AUTH_MODE` env var on the sandbox (set it via
`sandbox.extraEnv`):

| Mode | Upstream | Secrets required |
|------|----------|------------------|
| `api_key` (default) | `api.anthropic.com` | `ANTHROPIC_API_KEY` |
| `access_token` | `api.anthropic.com` | `CLAUDE_CODE_CLIENT_ID`, `CLAUDE_CODE_BLOB` |

`access_token` mode routes Claude Code through a Claude.ai Pro or Max
subscription rather than a raw API key. [iron-token-broker](https://docs.iron.sh)
holds the refresh token and mints short-lived access tokens, which iron-proxy
injects on outbound requests so the sandbox never sees them. The entrypoint
plants a dummy `~/.claude/.credentials.json` so the CLI emits OAuth-shaped
requests; the broker overwrites the Bearer at request time.

Store these two items in your secrets backend (1Password vault, Kubernetes
Secret, etc.) when running in `access_token` mode:

- `CLAUDE_CODE_CLIENT_ID`: the Claude Code CLI's OAuth client id. This
  is a fixed, publicly known constant:
  `9d1c250a-e61b-44d9-88ed-5944d1962f5e`. It is the same for every Claude
  Code install and never rotates, but the broker still resolves it through
  your secrets backend, so store the literal value as-is.
- `CLAUDE_CODE_BLOB`: a JSON document `{"refresh_token": "..."}`. The
  broker rotates this in place on every refresh, so the backing item must be
  writable.

To bootstrap, run `claude login` locally, then copy the refresh token from
`~/.claude/.credentials.json` (or from the `Claude Code-credentials` keychain
item on macOS) into `CLAUDE_CODE_BLOB`. Use the constant above for
`CLAUDE_CODE_CLIENT_ID`.

## 4. Configure Slack

Create the Slackbot app at [api.slack.com/apps](https://api.slack.com/apps).
Use the app page to install the bot, copy the Bot User OAuth Token for
`SLACK_BOT_TOKEN`, and copy the Signing Secret for `SLACK_SIGNING_SECRET`.

1. Add the bot scopes required by the Slackbot features you enable.
2. Install the app to the workspace.
3. Store the Bot User OAuth Token as `SLACK_BOT_TOKEN`.
4. Store the app Signing Secret as `SLACK_SIGNING_SECRET`.
5. Enable Event Subscriptions.
6. Set the Request URL to `https://<your-host>/api/webhooks/slack`.
7. Subscribe to `app_mention` and to the message events you want Centaur to see:
   `message.channels`, `message.groups`, and `message.im`.

The Slackbot currently normalizes Slack `app_mention` and `message` events.
Do not rely on assistant-specific Slack event types unless the Slackbot code has
explicit support for them.

Do not put Centaur API-key auth in front of `/api/webhooks/slack`; the Slackbot
validates Slack's signature and then calls the Centaur API separately.

The Slackbot accepts Slack events at `/api/webhooks/slack`. It also registers
compatibility paths for `/api/slack/events`, `/api/slack/actions`,
`/api/slack/options`, and `/api/slack/commands`.

## 5. Deploy with Helm

The chart lives at `contrib/chart`. Select service images, [iron-proxy](https://docs.iron.sh) secret
source, sandbox image, and optional runtime class in your values file:

```yaml
secretManager:
  existingSecretName: centaur-infra-env
  envPrefix: ""

api:
  executionWorkerEnabled: true
  warmPoolEnabled: true

ironProxy:
  secretSource: onepassword-connect
  secretTtl: 10m

apiRs:
  # Delete any sandbox older than this, running or suspended.
  sandboxMaxLifetimeSecs: 259200

onepasswordConnect:
  connect:
    create: true
    credentialsName: centaur-onepassword-connect-credentials
    credentialsKey: 1password-credentials.json

sandbox:
  image:
    repository: centaur-agent
    tag: latest
    pullPolicy: IfNotPresent
  runtimeClassName: gvisor
```

The Kubernetes sandbox backend is the active runtime backend; there is no chart
switch named `api.sandboxBackend`.

Sandbox lifecycle has two separate timers:

- Slackbot v2 sends `idle_timeout_ms` on execute requests, defaulting to up to
  3 hours, so api-rs can pause an idle sandbox after a turn finishes.
- api-rs deletes old sandboxes through `apiRs.sandboxMaxLifetimeSecs`, default
  72 hours, regardless of whether the sandbox is still running or already
  suspended.

There is no suspended-only delete setting. If you want sandboxes gone after N
hours, set `apiRs.sandboxMaxLifetimeSecs` to N hours in seconds.

Install or upgrade:

```bash
helm lint contrib/chart
helm upgrade --install centaur contrib/chart \
  --namespace centaur-system \
  --create-namespace \
  -f values.production.yaml
```

## 6. Verify the deployment

Check health from inside the api-rs deployment first:

```bash
kubectl exec -n centaur-system deploy/centaur-centaur-api-rs -- \
  curl -fsS http://localhost:8080/healthz

kubectl exec -n centaur-system deploy/centaur-centaur-api-rs -- \
  curl -fsS http://localhost:8080/readyz | jq
```

Run one agent turn from inside the api-rs deployment:

```bash
THREAD_KEY=cli:production-smoke-codex
THREAD_PATH=$(jq -rn --arg v "$THREAD_KEY" '$v|@uri')

SESSION=$(kubectl exec -n centaur-system deploy/centaur-centaur-api-rs -- curl -s -X POST "http://localhost:8080/api/session/${THREAD_PATH}" \
  -H "Content-Type: application/json" \
  -d '{"harness_type":"codex","on_harness_conflict":"restart"}')

kubectl exec -n centaur-system deploy/centaur-centaur-api-rs -- curl -s -X POST "http://localhost:8080/api/session/${THREAD_PATH}/messages" \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","parts":[{"type":"text","text":"Reply with exactly PONG."}]}]}'

EXECUTE=$(kubectl exec -n centaur-system deploy/centaur-centaur-api-rs -- curl -s -X POST "http://localhost:8080/api/session/${THREAD_PATH}/execute" \
  -H "Content-Type: application/json" \
  -d '{"input_lines":["{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Reply with exactly PONG.\"}]}}"]}')
EXECUTION_ID=$(printf '%s' "$EXECUTE" | jq -r '.execution_id')

kubectl exec -n centaur-system deploy/centaur-centaur-api-rs -- curl -s -N \
  "http://localhost:8080/api/session/${THREAD_PATH}/events?execution_id=${EXECUTION_ID}&after_event_id=0"
```

Then run the same prompt through Slack:

```text
reply with exactly PONG
```

Slack messages without a harness flag use Codex. Use `--amp`, `--claude`,
`--codex`, or `--pi` only when you want to select a specific harness.

Inspect sandbox pods with the labels Centaur actually sets:

```bash
kubectl get pods -n centaur-system -l centaur.ai/managed=true
kubectl exec -n centaur-system <agent-sandbox-pod> -- centaur-tools list
```

If a run fails because the sandbox pod exits or is deleted, inspect the durable
session and api-rs logs before retrying:

```bash
kubectl exec -n centaur-system deploy/centaur-centaur-api-rs -- curl -s \
  "http://localhost:8080/api/session/${THREAD_PATH}" | jq

kubectl logs -n centaur-system deploy/centaur-centaur-api-rs --tail=200
kubectl get pods -n centaur-system -l centaur.ai/managed=true
```

Centaur preserves the execution row and event trail; retry by starting a new
turn after you understand whether the failure was credentials, image pull,
network policy, harness startup, or the upstream model/tool call.

## 7. Keep the operating loop small

Before expanding the deployment, record:

1. The operator.
2. Where secrets live.
3. How to restart the stack.
4. The first working Slack channel.
5. The enabled harnesses.
6. The first useful tool or workflow.
7. How to inspect logs and failed runs.

The operator's job is to leave behind a repeatable operating loop, not a
one-time demo.
