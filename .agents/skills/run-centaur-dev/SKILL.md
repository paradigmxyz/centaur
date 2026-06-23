---
name: run-centaur-dev
description: "Bring up the full Centaur development stack locally with cargo-run Rust API, Kind agent sandboxes, onepassword/iron-proxy credentials, Tailscale Funnel Slack webhooks, Slack app manifest setup, and centaur-session-cli smoke tests or thread attachment."
---

# Run Centaur Dev

Use this when the user asks to run Centaur locally end-to-end with the Rust API, real agent sandboxes, Slackbot v2, Tailscale Funnel, or `centaur-session-cli`.

## Ground Rules

- Stay local. Do not SSH to the deploy box and do not touch production unless explicitly asked.
- Use `services/api-rs` for the Rust control plane. The API should run on the host with `cargo run`; sandboxes run in Kind.
- Prefer onepassword/iron-proxy for model/tool credentials. Do not pass real LLM keys into sandbox env unless the user explicitly asks for a shortcut.
- Never print secret values. Use `op read`/`op item get` into env vars and only log which secret names were used.
- Before branch changes or cleanup, check `git status --short --branch` and preserve unrelated dirty files.

## Prereqs

Required CLIs: `cargo`, `docker`, `kind`, `kubectl`, `just`, `bun`, `tailscale`, `op`.

If 1Password is not signed in, run `op signin`. The usual vaults are `centaur-agent` for agent/iron-proxy credentials and `prd-centaur-infra` for deployed Slack app or infra references. Do not guess item paths; discover them:

```bash
op item list --vault centaur-agent | rg -i 'slack|openai|anthropic|amp|iron|broker'
op item list --vault prd-centaur-infra | rg -i 'slack|centaur'
```

## 1. Start Kind Agent Sandbox Infra

From repo root:

```bash
cd services/api-rs
just kind-e2e-up
```

This creates/uses `kind-centaur-api-rs-e2e`, installs the Agent Sandbox CRD/controller, and creates namespace `centaur-sandbox-e2e`.

Build the images the sandbox backend needs, then load them only if the Kind node does not already have them:

```bash
just kind-e2e-build-images
just kind-e2e-load-images
```

`kind-e2e-load-images` skips `kind load` when `centaur-agent:latest` and `centaur-iron-proxy:latest` are already present in the Kind node. Re-run with `KIND_E2E_FORCE_IMAGE_LOAD=1 just kind-e2e-load-images` only after changing the sandbox image, iron-proxy image, or baked sandbox skills/prompts. If you keep the Kind cluster around, the images stay around too; deleting the Kind cluster deletes that node-local image cache.

Bootstrap namespace secrets for the sandbox namespace. Set real values first; `SLACKBOT_API_KEY` can be a local random service key because api-rs currently has no auth middleware.

```bash
export OP_SERVICE_ACCOUNT_TOKEN="$(op read 'op://centaur-agent/<item>/<field>')"
export OP_VAULT=centaur-agent
export SLACK_BOT_TOKEN="$(op read 'op://<vault>/<slack-item>/<bot-token-field>')"
export SLACK_SIGNING_SECRET="$(op read 'op://<vault>/<slack-item>/<signing-secret-field>')"
export SLACKBOT_API_KEY="${SLACKBOT_API_KEY:-$(openssl rand -hex 32)}"

CENTAUR_NAMESPACE=centaur-sandbox-e2e just bootstrap-secrets
```

Verify:

```bash
kubectl --context kind-centaur-api-rs-e2e -n agent-sandbox-system rollout status deploy/agent-sandbox-controller --timeout=120s
kubectl --context kind-centaur-api-rs-e2e -n centaur-sandbox-e2e get secrets centaur-infra-env centaur-firewall-ca centaur-firewall-ca-key
```

## 2. Start Postgres

For host-run api-rs, use a host-reachable Postgres. A disposable local container is usually fastest:

```bash
docker rm -f centaur-api-rs-postgres 2>/dev/null || true
docker run --name centaur-api-rs-postgres \
  -e POSTGRES_USER=postgres \
  -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=centaur \
  -p 5432:5432 \
  -d postgres:16

export DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/centaur
```

If port 5432 is occupied, map another host port and update `DATABASE_URL`.

## 3. Run api-rs On The Host

Pick the host URL that Kind pods can reach. On Docker Desktop this is usually `host.docker.internal`; on OrbStack it may be `host.orb.internal`.

```bash
export SANDBOX_HOST_API_URL=http://host.docker.internal:8080
```

Run the API:

```bash
cd services/api-rs
RUST_LOG=info \
DATABASE_URL="$DATABASE_URL" \
RUN_MIGRATIONS=true \
BIND_ADDR=0.0.0.0:8080 \
SESSION_SANDBOX_K8S_CONTEXT=kind-centaur-api-rs-e2e \
SESSION_SANDBOX_K8S_NAMESPACE=centaur-sandbox-e2e \
SESSION_SANDBOX_BACKEND=agent-k8s \
SESSION_SANDBOX_WORKLOAD=codex-app-server \
SESSION_SANDBOX_IMAGE=centaur-agent:latest \
SESSION_SANDBOX_IMAGE_PULL_POLICY=IfNotPresent \
KUBERNETES_SANDBOX_IRON_PROXY_MODE=enabled \
KUBERNETES_IRON_PROXY_IMAGE_PULL_POLICY=IfNotPresent \
KUBERNETES_FIREWALL_CA_SECRET_NAME=centaur-firewall-ca \
KUBERNETES_FIREWALL_CA_KEY_SECRET_NAME=centaur-firewall-ca-key \
FIREWALL_MANAGER_SECRET_SOURCE=onepassword \
KUBERNETES_BOOTSTRAP_SECRET_NAME=centaur-infra-env \
OP_VAULT="${OP_VAULT:-centaur-agent}" \
TOOL_DIRS="$PWD/../../tools" \
REPOS_PATH="$HOME/paradigmxyz" \
SESSION_SANDBOX_CENTAUR_API_URL="$SANDBOX_HOST_API_URL" \
CODEX_AUTH_MODE=api_key \
cargo run -p centaur-api-server
```

In another shell:

```bash
curl -sS http://127.0.0.1:8080/healthz
kubectl --context kind-centaur-api-rs-e2e -n centaur-sandbox-e2e run api-check --rm -i --restart=Never --image=curlimages/curl -- \
  curl -sS "$SANDBOX_HOST_API_URL/healthz"
```

If the pod check fails, switch `SANDBOX_HOST_API_URL` between `host.docker.internal` and `host.orb.internal`, restart api-rs, and retry.

## 4. Run Slackbot V2 Locally

In a separate shell:

```bash
cd services/slackbotv2
PORT=3002 \
CENTAUR_API_URL=http://127.0.0.1:8080 \
SLACK_BOT_TOKEN="$SLACK_BOT_TOKEN" \
SLACK_SIGNING_SECRET="$SLACK_SIGNING_SECRET" \
SLACKBOT_API_KEY="$SLACKBOT_API_KEY" \
SLACKBOTV2_DATABASE_URL="$DATABASE_URL" \
bun run dev
```

Check:

```bash
curl -sS http://127.0.0.1:3002/health
```

## 5. Expose Slackbot With Tailscale Funnel

```bash
tailscale funnel --bg --yes 3002
tailscale funnel status
```

Use the reported HTTPS URL plus `/api/webhooks/slack` as the Slack Request URL:

```text
https://<machine>.<tailnet>.ts.net/api/webhooks/slack
```

Stop/reset when done:

```bash
tailscale funnel reset
```

## 6. Slack App Manifest

Use the Slack app UI manifest editor or Slack CLI. Set the Request URL to the Funnel URL from the previous step, reinstall the app after manifest changes, then refresh `SLACK_BOT_TOKEN` and `SLACK_SIGNING_SECRET` if Slack rotated them.

Minimal manifest shape:

```yaml
display_information:
  name: Centaur Dev
features:
  bot_user:
    display_name: centaur-dev
    always_online: true
oauth_config:
  scopes:
    bot:
      - app_mentions:read
      - assistant:write
      - channels:history
      - channels:read
      - chat:write
      - groups:history
      - groups:read
      - im:history
      - im:read
      - mpim:history
      - mpim:read
      - users:read
settings:
  event_subscriptions:
    request_url: https://<machine>.<tailnet>.ts.net/api/webhooks/slack
    bot_events:
      - app_mention
      - message.channels
      - message.groups
      - message.im
      - message.mpim
  interactivity:
    is_enabled: true
    request_url: https://<machine>.<tailnet>.ts.net/api/webhooks/slack
  org_deploy_enabled: false
  socket_mode_enabled: false
  token_rotation_enabled: false
```

Do not put API-key auth in front of `/api/webhooks/slack`; Slackbot validates Slack signatures and then calls api-rs.

## 7. Use centaur-session-cli

Build/run from `services/api-rs`.

Create a new thread in the TUI:

```bash
CENTAUR_API_URL=http://127.0.0.1:8080 \
cargo run -p centaur-session-cli -- --tui --harness-type codex
```

The CLI prints `thread_key=cli:<uuid>` for new threads. In the TUI, type a message and submit it.

Create a new non-TUI smoke turn:

```bash
CENTAUR_API_URL=http://127.0.0.1:8080 \
cargo run -p centaur-session-cli -- \
  --harness-type codex \
  --message "Reply with exactly PONG and nothing else." \
  --exit-on-terminal \
  --max-duration-ms 120000
```

Follow a Slack thread by durable thread key:

```bash
CENTAUR_API_URL=http://127.0.0.1:8080 \
cargo run -p centaur-session-cli -- \
  --attach \
  --thread-key 'slack:<CHANNEL_ID>:<THREAD_TS>' \
  --all-events
```

Attach with TUI:

```bash
CENTAUR_API_URL=http://127.0.0.1:8080 \
cargo run -p centaur-session-cli -- \
  --attach \
  --thread-key 'slack:<CHANNEL_ID>:<THREAD_TS>' \
  --tui
```

Resume from an event id:

```bash
cargo run -p centaur-session-cli -- \
  --thread-key 'slack:<CHANNEL_ID>:<THREAD_TS>' \
  --after-event-id <LAST_EVENT_ID> \
  --all-events
```

For non-TUI interactive input, add `--stdin-events`. Lines are messages by default; `/message <text>`, `/input <raw-json-line>`, `/execute <json-array-or-line>`, and `/quit` are supported.

## 8. Smoke The Full Path

1. Mention the Slack app in the configured workspace/channel.
2. Confirm Slackbot logs show `slackbotv2_forward_started`.
3. Confirm api-rs logs show session creation/execution and no mock workload.
4. Confirm Kind created an Agent Sandbox:

```bash
kubectl --context kind-centaur-api-rs-e2e -n centaur-sandbox-e2e get sandboxes,pods
```

5. Attach to the Slack thread key with `centaur-session-cli` and verify events replay.

## Troubleshooting

- Mock output instead of Codex: restart api-rs with `SESSION_SANDBOX_BACKEND=agent-k8s` and `SESSION_SANDBOX_WORKLOAD=codex-app-server`.
- Sandbox cannot call API/tools: verify `SANDBOX_HOST_API_URL` from inside a Kind pod and restart api-rs with the working value in `SESSION_SANDBOX_CENTAUR_API_URL`.
- Agent or iron-proxy image pull failure after changing images: rebuild and run `KIND_E2E_FORCE_IMAGE_LOAD=1 just kind-e2e-load-images`; keep `SESSION_SANDBOX_IMAGE_PULL_POLICY=IfNotPresent` and `KUBERNETES_IRON_PROXY_IMAGE_PULL_POLICY=IfNotPresent`.
- Iron-proxy missing CA: rerun `CENTAUR_NAMESPACE=centaur-sandbox-e2e just bootstrap-secrets` and verify `centaur-firewall-ca` plus `centaur-firewall-ca-key` exist.
- Model auth failure: check api-rs sandbox env says `CODEX_AUTH_MODE=api_key`, iron-proxy is enabled, and `FIREWALL_MANAGER_SECRET_SOURCE=onepassword` has the expected `OP_SERVICE_ACCOUNT_TOKEN`/`OP_VAULT` in `centaur-infra-env`.
- Slack does not reach the bot: `tailscale funnel status`, Slack Request URL must end in `/api/webhooks/slack`, and `SLACK_SIGNING_SECRET` must match the app.
- Slackbot receives events but does not stream: check `SLACKBOTV2_DATABASE_URL`, Slack `assistant:write` scope, and `chat.startStream`/`chat.appendStream` errors in Slackbot logs.
- Re-run sandbox invariant tests when changing sandbox/runtime behavior:

```bash
cd services/api-rs
just e2e-kind
```
