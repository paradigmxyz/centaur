---
title: Use 1Password
description: Configure Centaur to resolve tool and harness credentials from 1Password through iron-proxy.
---

# Use 1Password

Use 1Password when you want tool and harness credentials to stay out of sandbox
pods and out of the API process. Sandboxes receive placeholders. [iron-proxy](https://docs.iron.sh)
resolves the real credential and injects it only for allowed upstream hosts.

## Configure the chart

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
[tool.ai-v2]
hosts = ["warehouse.internal.example.com"]
secrets = ["WAREHOUSE_API_KEY"]
```

Create a 1Password item named `WAREHOUSE_API_KEY` in `OP_VAULT`, with the value
stored in the `credential` field. [iron-proxy](https://docs.iron.sh) resolves:

```text
op://$OP_VAULT/WAREHOUSE_API_KEY/credential
```

The tool sees `WAREHOUSE_API_KEY` as a placeholder. For requests to
`warehouse.internal.example.com`, [iron-proxy](https://docs.iron.sh) replaces that placeholder with the
real 1Password value.

## Harness credentials

Store enabled harness credentials the same way:

| Credential | Used for |
|------------|----------|
| `OPENAI_API_KEY` | Codex default |
| `AMP_API_KEY` | Amp |
| `ANTHROPIC_API_KEY` | Claude Code and pi-mono |

Each item should live in `OP_VAULT` with its value in `credential`.

## 1Password Connect

Use service-account mode (`ironProxy.secretSource: onepassword`) unless your
cluster already standardizes on an in-cluster 1Password Connect deployment.
Connect mode is useful when operators want all 1Password access to go through a
Connect server and token instead of direct service-account resolution from the
proxy.

If your cluster should run 1Password Connect with this chart, use:

```yaml
ironProxy:
  secretSource: onepassword-connect

onepasswordConnect:
  connect:
    create: true
    credentialsName: centaur-onepassword-connect-credentials
    credentialsKey: 1password-credentials.json
```

This mode resolves the same `op://$OP_VAULT/<secret_ref>/credential` references,
but through the in-cluster Connect service.

The credentials Secret must contain `1password-credentials.json`; local
bootstrap creates it when `OP_CONNECT_CREDENTIALS_FILE` points at that file. The
infra Secret must also include `OP_CONNECT_TOKEN`.

## Verify

Check that the API and [iron-proxy](https://docs.iron.sh) received the expected source mode:

```bash
kubectl exec -n centaur-system deploy/centaur-centaur-api -- env | \
  grep -E 'FIREWALL_MANAGER_SECRET_SOURCE|OP_VAULT'
```

For Connect mode, also verify the Connect pod and token Secret exist:

```bash
kubectl get pods -n centaur-system -l app.kubernetes.io/name=connect
kubectl get secret -n centaur-system centaur-onepassword-connect-credentials
kubectl get secret -n centaur-system centaur-infra-env -o jsonpath='{.data.OP_CONNECT_TOKEN}' >/dev/null
```

Then run a tool or harness call that reaches an allowed host. If injection
fails, check the tool's `hosts`, declared `secrets`, item name, `OP_VAULT`, and
whether the item has a `credential` field.
