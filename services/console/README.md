# iron-control

`iron-control` is the control plane for [`iron-proxy`](https://github.com/ironsh/iron-proxy), a forwarding proxy that adds credentials to outbound HTTP requests. Applications route their traffic through `iron-proxy` and never hold the secrets themselves. `iron-control` stores the credentials, decides which proxy may use which credential on which requests, and hands each proxy its configuration.

It is a Rails application backed by Postgres. It provides a JSON API, an operator console, and encryption at rest for the secrets it stores.

## What It Does

- **Stores credentials.** Each secret is a typed record. The value is either kept inline and encrypted, or pulled from an external store such as AWS Secrets Manager, AWS SSM, 1Password, or an environment variable. The supported kinds are static secrets, GCP service-account auth, AWS SigV4 auth, OAuth tokens, Postgres connection strings, and HMAC signing keys.
- **Controls who can use them.** A **principal** is an identity that a proxy runs as. A **role** groups credentials so they can be assigned together. A **grant** gives one credential to a principal or a role. A principal can use its own grants plus the grants of every role it has.
- **Limits where they apply.** Each grant has request rules for host, methods, and paths, so a credential is only added to the requests it is meant for.
- **Configures proxies.** A **proxy** registers with `iron-control`, gets assigned a principal, and calls `POST /api/v1/proxy/sync` to fetch its configuration. The response includes a config hash that works like an ETag, so a proxy that already has the current config gets an empty response.
- **Issues short-lived tokens.** For `token_broker` credentials, `iron-control` mints and rotates the access token itself and sends only the token to the proxy. The underlying credential never leaves the control plane.

## How It Fits Together

```
  operator ──▶ console / JSON API ──▶ iron-control ──▶ Postgres (encrypted secrets)
                                           ▲
                                           │ POST /api/v1/proxy/sync (iprx_ token)
                                           │
  application ──▶ iron-proxy ──────────────┘
                     │
                     └──▶ upstream APIs (credentials added per request rules)
```

Operators manage credentials, principals, roles, and grants through the API or the console. Each `iron-proxy` instance signs in with its own token, fetches the configuration for its assigned principal, and adds the granted credentials to matching outbound requests.

## Environment Variables

All of the console's environment variables use the `CENTAUR_CONSOLE_` prefix. For backwards compatibility, every variable also resolves from the legacy `IRON_CONTROL_` name when the `CENTAUR_CONSOLE_` one is unset, so existing deployments keep working until they migrate. The `CENTAUR_CONSOLE_` name wins when both are set.

The Threads tab reads api-rs session rows from the Centaur API database. Set
`CENTAUR_CONSOLE_CENTAUR_DATABASE_URL` to that database URL when it differs from
the console's primary database. In the Helm chart this is sourced from the
shared `DATABASE_URL` secret key.

For local development, sign in through the normal login form at
`http://localhost:3000/login` with the seeded initial user's
`CENTAUR_CONSOLE_INITIAL_USER_EMAIL` / `CENTAUR_CONSOLE_INITIAL_USER_PASSWORD`
credentials, the same as every other environment.

To build the Threads UX against production-shaped data without connecting the
Console to production, create a bounded local snapshot:

```bash
export CENTAUR_PROD_DATABASE_URL=postgresql://readonly:...@.../ai_v2
scripts/mirror-prod-threads-snapshot.sh all
```

The script exports recent `sessions`, `session_messages`,
`session_executions`, terminal `session_events` plus reasoning
`session.output.line` events (capped by `THINKING_EVENT_LIMIT_PER_THREAD`,
default 200 per thread), and referenced `slack_sync_users` rows with the source
connection forced read-only, then imports them into the local `ai_v2` database
used by the Console dev container. The Threads surface is read-only: it does
not render a composer and rejects POSTs server-side.

Threads extras beyond the Slack surface:

- Thinking traces: reasoning items the harness streamed over stdout are
  persisted by api-rs as `session.output.line` events; the transcript renders
  each completed reasoning block as a collapsed "Thinking" disclosure.
- Split view: Cmd/Ctrl-click a sidebar thread to open it alongside the current
  one, up to four threads in a grid. The `thread` param carries the open keys
  comma-separated (`?thread=<primary>,<key2>,<key3>,<key4>`), primary first.
  All keys resolve through the same owner scope as a single thread, and each
  panel has a close control.

## First Boot

The console requires an authenticated user and API key before any API endpoint will respond. To bootstrap a fresh deployment without a console, set the following environment variables on startup:

| Variable                          | Required | Description                                                                                              |
| --------------------------------- | -------- | -------------------------------------------------------------------------------------------------------- |
| `CENTAUR_CONSOLE_INITIAL_USER_EMAIL`    | yes      | Email for the initial user.                                                                              |
| `CENTAUR_CONSOLE_INITIAL_USER_PASSWORD` | yes      | Password for the initial user (minimum 12 characters).                                                   |
| `CENTAUR_CONSOLE_INITIAL_API_KEY`       | no       | Plaintext API key for the initial user. Must match `iak_` followed by 64 lowercase hex characters (a 32-byte hex string). If omitted, a token is generated and logged once at startup. |

Behavior:

- Bootstrap runs after Rails initialization on every boot, but is a no-op if any user already exists. It is safe to leave the env vars set across rolling restarts.
- If `CENTAUR_CONSOLE_INITIAL_USER_EMAIL` is set without `CENTAUR_CONSOLE_INITIAL_USER_PASSWORD`, the process exits with a clear error.
- Concurrent pods racing the first boot are serialized with a Postgres advisory lock; exactly one user is created.

When deploying to Kubernetes, source these values from a `Secret`, not from a `ConfigMap`.

## Google And Slack Authentication

The operator console always supports email and password sign-in. To add Google or Slack SSO buttons to the login page, configure an OAuth/OIDC client with the provider and set the matching client credentials in the environment. A provider is shown only when both its client ID and client secret are present.

| Variable                              | Required | Description                                                                                 |
| ------------------------------------- | -------- | ------------------------------------------------------------------------------------------- |
| `CENTAUR_CONSOLE_PUBLIC_URL`             | recommended | Public origin for this deployment, for example `https://control.example.com`. Use this when `iron-control` runs behind a proxy or load balancer whose internal host does not match the public URL. |
| `CENTAUR_CONSOLE_GOOGLE_CLIENT_ID`       | for Google | Google OAuth client ID for console login.                                                    |
| `CENTAUR_CONSOLE_GOOGLE_CLIENT_SECRET`   | for Google | Google OAuth client secret for console login.                                                |
| `CENTAUR_CONSOLE_SLACK_CLIENT_ID`        | for Slack | Slack OpenID Connect client ID for console login.                                            |
| `CENTAUR_CONSOLE_SLACK_CLIENT_SECRET`    | for Slack | Slack OpenID Connect client secret for console login.                                        |
| `CENTAUR_CONSOLE_BOOTSTRAP_ADMINS`       | no       | Comma- or whitespace-separated email allowlist. Matching users become active admins on first SSO login. Other new SSO users are created as pending users. |

Register these callback URLs with the provider:

- Google: `<CENTAUR_CONSOLE_PUBLIC_URL>/auth/google/callback`
- Slack: `<CENTAUR_CONSOLE_PUBLIC_URL>/auth/slack/callback`

Both providers request the `openid`, `email`, and `profile` scopes. Client credentials may also be stored in Rails credentials under `console_auth.<provider>.client_id` and `console_auth.<provider>.client_secret`, but environment variables take precedence.

Google OAuth consent apps for broker credentials are configured separately in the console under **OAuth Apps**. Those app callbacks use `/oauth/<slug>/callback` and currently support Google only; Slack support here applies to operator console sign-in.

## Encryption Keys

`iron-control` uses ActiveRecord encryption to protect secrets stored in the control plane (for example, the `control_plane` secret source type). The following environment variables configure the encryption keys:

| Variable                                 | Required           | Description                                  |
| ---------------------------------------- | ------------------ | -------------------------------------------- |
| `CENTAUR_CONSOLE_AR_ENCRYPTION_PRIMARY_KEY`         | yes (in production) | Primary key used for non-deterministic encryption. |
| `CENTAUR_CONSOLE_AR_ENCRYPTION_DETERMINISTIC_KEY`   | yes (in production) | Key used for deterministic encryption.       |
| `CENTAUR_CONSOLE_AR_ENCRYPTION_KEY_DERIVATION_SALT` | yes (in production) | Salt used to derive per-attribute keys.      |

Generate suitable values with `bin/rails db:encryption:init` and store them in your secret manager. In production, the process refuses to boot if any of the three are missing. In `development` and `test`, fixed fallback values are used so the suite runs without configuration.

Rotating any of these keys makes previously encrypted data unreadable. Treat them as long-lived secrets and back them up alongside other production credentials.

## API

`iron-control` exposes a JSON API under `/api/v1`. All resource endpoints authenticate with an API key sent as a bearer token (`Authorization: Bearer iak_...`); the one exception is `POST /api/v1/proxy/sync`, which `iron-proxy` instances call with a proxy bearer token.

See [docs/API.md](docs/API.md) for the full reference: authentication, request/response conventions, pagination, error formats, the shared secret-source and request-rule shapes, and detailed payloads for every endpoint (static secrets, GCP auth secrets, OAuth token secrets, principals, roles, grants, API keys, proxies, and proxy sync).
