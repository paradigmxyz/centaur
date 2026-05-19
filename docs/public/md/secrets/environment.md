---
title: Use Environment Variables
description: Configure Centaur to resolve tool and harness credentials from Kubernetes environment secrets.
---

# Use Environment Variables

Environment-backed secrets are the simplest secret source. [iron-proxy](https://docs.iron.sh) reads real
credential values from environment variables on the proxy container.

Use this for local development, CI, or simple private deployments. For
production, prefer 1Password if you do not want tool credentials stored directly
in a Kubernetes Secret.

## Configure the chart

```yaml
ironProxy:
  secretSource: env

secretManager:
  existingSecretName: centaur-infra-env
  envPrefix: ""
```

Put infrastructure secrets and tool credentials in the Secret selected by
`secretManager.existingSecretName`.

```bash
kubectl create secret generic centaur-infra-env \
  --namespace centaur-system \
  --from-literal=DATABASE_URL='postgres://...' \
  --from-literal=SLACKBOT_API_KEY='...' \
  --from-literal=SLACK_BOT_TOKEN='xoxb-...' \
  --from-literal=SLACK_SIGNING_SECRET='...' \
  --from-literal=SANDBOX_SIGNING_KEY="$(openssl rand -hex 32)" \
  --from-literal=IRON_MANAGEMENT_API_KEY="$(openssl rand -hex 32)" \
  --from-literal=OPENAI_API_KEY='...' \
  --from-literal=AMP_API_KEY='...' \
  --from-literal=ANTHROPIC_API_KEY='...' \
  --from-literal=WAREHOUSE_API_KEY='...'
```

For local development, `just bootstrap-secrets` creates the local Kubernetes
Secret from your shell environment.

## How tool secrets resolve

For:

```toml
secrets = ["WAREHOUSE_API_KEY"]
```

Centaur uses:

- secret name: `WAREHOUSE_API_KEY`
- secret reference: `WAREHOUSE_API_KEY`
- placeholder value seen by the tool: `WAREHOUSE_API_KEY`

In `env` mode, [iron-proxy](https://docs.iron.sh) reads the real value from the environment variable
named by the secret reference.

## Advanced secret entries

Most tools should use the string form. The parser also supports explicit secret
tables for special cases:

```toml
[[tool.ai-v2.secrets]]
type = "gcp_auth"
name = "ANALYTICS_BIGQUERY_CREDENTIAL"
secret_ref = "ANALYTICS_BIGQUERY_CREDENTIAL"

[[tool.ai-v2.secrets]]
type = "pg_dsn"
name = "WAREHOUSE_POSTGRES_DSN"
secret_ref = "WAREHOUSE_POSTGRES_DSN"
database = "analytics"
```

Use `gcp_auth` when [iron-proxy](https://docs.iron.sh) should mint Google OAuth tokens for Google APIs.
Use `pg_dsn` when a sandbox needs a proxied Postgres DSN instead of a raw
database URL.

## Verify

Check the API pod environment:

```bash
kubectl exec -n centaur-system deploy/centaur-centaur-api -- env | \
  grep -E 'FIREWALL_MANAGER_SECRET_SOURCE|WAREHOUSE_API_KEY'
```

Then call a tool that uses the secret and check that the upstream request works.
If it fails, check the Kubernetes Secret key name, `ironProxy.secretSource`,
tool `hosts`, and the declared `secrets`.
