---
title: Use 1Password
description: Configure Centaur to resolve tool and harness credentials from 1Password through iron-proxy.
---

# Use 1Password

Use 1Password when you want tool and harness credentials to stay out of sandbox
pods and out of the API process. Sandboxes receive placeholders. [iron-proxy](https://docs.iron.sh)
resolves the real credential and injects it only for allowed upstream hosts.

There are two source modes:

- `onepassword-connect` runs an in-cluster 1Password Connect server and has
  iron-proxy talk to it with a Connect token. **This is the preferred mode
  for production**, mostly because the service-account SDK is rate-limited
  by 1Password and Connect is not. Under any non-trivial agent load you
  will hit those limits with the SDK. Connect resolves locally against the
  in-cluster server and stays out of the way.
- `onepassword` uses a 1Password service-account token directly from
  iron-proxy. Simpler to set up and fine for local development or low-volume
  deployments, but expect throttling once real traffic shows up.

## Configure the chart (Connect, preferred)

```yaml
ironProxy:
  secretSource: onepassword-connect
  secretTtl: 10m

onepasswordConnect:
  connect:
    create: true
    credentialsName: centaur-onepassword-connect-credentials
    credentialsKey: 1password-credentials.json

secretManager:
  existingSecretName: centaur-infra-env
  envPrefix: ""
```

The credentials Secret must contain `1password-credentials.json`; local
bootstrap creates it when `OP_CONNECT_CREDENTIALS_FILE` points at that file.
The infra Secret must include:

```text
OP_CONNECT_TOKEN
OP_VAULT
```

## Configure the chart (service account)

```yaml
ironProxy:
  secretSource: onepassword
  secretTtl: 10m

secretManager:
  existingSecretName: centaur-infra-env
  envPrefix: ""
```

The infra Secret must include:

```text
OP_SERVICE_ACCOUNT_TOKEN
OP_VAULT
```

It must also include infrastructure secrets such as:

```text
DATABASE_URL
SLACKBOT_API_KEY
SLACK_BOT_TOKEN
SLACK_SIGNING_SECRET
SANDBOX_SIGNING_KEY
IRON_MANAGEMENT_API_KEY
```

Those are boot-time service secrets, not tool credentials.

## Name 1Password items

For the normal tool declaration:

```toml
[tool.centaur]
secrets = [
    {type = "http", name = "WAREHOUSE_API_KEY", match_headers = ["Authorization"], hosts = ["warehouse.internal.example.com"]},
]
```

Create a 1Password item named `WAREHOUSE_API_KEY` in `OP_VAULT`, with the value
stored in the `credential` field. [iron-proxy](https://docs.iron.sh) resolves:

```text
op://$OP_VAULT/WAREHOUSE_API_KEY/credential
```

The tool sees `WAREHOUSE_API_KEY` as a placeholder. For requests to
`warehouse.internal.example.com` whose `Authorization` header contains the
placeholder, [iron-proxy](https://docs.iron.sh) replaces it with the real
1Password value.

## Harness credentials

Store enabled harness credentials the same way:

| Credential | Used for |
|------------|----------|
| `OPENAI_API_KEY` | Codex default |
| `OPENROUTER_API_KEY` | OpenRouter via Codex |
| `META_AI_API_KEY` | Meta AI direct via Codex |
| `AMP_API_KEY` | Amp |
| `ANTHROPIC_API_KEY` | Claude Code and pi-mono |

Each item should live in `OP_VAULT` with its value in `credential`.

## Verify

Check that the API and [iron-proxy](https://docs.iron.sh) received the expected source mode:

```bash
kubectl exec -n centaur-system deploy/centaur-centaur-api-rs -- env | \
  grep -E 'FIREWALL_MANAGER_SECRET_SOURCE|OP_VAULT'
```

For Connect mode, also verify the Connect pod and token Secret exist:

```bash
kubectl get pods -n centaur-system -l app.kubernetes.io/name=connect
kubectl get secret -n centaur-system centaur-onepassword-connect-credentials
kubectl get secret -n centaur-system centaur-infra-env -o jsonpath='{.data.OP_CONNECT_TOKEN}' >/dev/null
```

Then run a tool or harness call that reaches an allowed host. If injection
fails, check the secret entry's `hosts` and `match_*` fields, the 1Password
item name, `OP_VAULT`, and whether the item has a `credential` field.
