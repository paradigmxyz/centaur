# iron-control

`iron-control` is the control plane for [`iron-proxy`](https://github.com/ironsh/iron-proxy), a forwarding proxy that adds credentials to outbound HTTP requests. Applications route their traffic through `iron-proxy` and never hold the secrets themselves. `iron-control` stores the credentials, decides which proxy may use which credential on which requests, and hands each proxy its configuration.

api-rs serves it from Postgres: a JSON API, an operator console, and encryption at rest for the secrets it stores. The server code is in [`crates/centaur-api-server/src/console/`](../../crates/centaur-api-server/src/console/README.md) and [`crates/centaur-console-db`](../../crates/centaur-console-db/); this directory holds the operator UI (`web/`), the static files api-rs serves (`public/`), and the API reference (`docs/API.md`).

## What It Does

- **Stores credentials.** Each secret is a typed record. The value is either kept inline and encrypted, or pulled from an external store such as AWS Secrets Manager, AWS SSM, 1Password, or an environment variable. The supported kinds are static secrets, GCP service-account auth, AWS SigV4 auth, OAuth tokens, Postgres connection strings, and HMAC signing keys.
- **Controls who can use them.** A **principal** is an identity that a proxy runs as. A **role** groups credentials so they can be assigned together. A **grant** gives one credential to a principal or a role. A principal can use its own grants plus the grants of every role it has.
- **Limits where they apply.** Each grant has request rules for host, methods, and paths, so a credential is only added to the requests it is meant for.
- **Configures proxies.** A **proxy** registers with `iron-control`, gets assigned a principal, and calls `POST /api/v1/proxy/sync` on the `proxy-sync` service, which reads `iron-control`'s database, to fetch its configuration. The response includes a config hash that works like an ETag, so a proxy that already has the current config gets an empty response.
- **Issues short-lived tokens.** For `token_broker` credentials, `iron-control` mints and rotates the access token itself and sends only the token to the proxy. The underlying credential never leaves the control plane.

## How It Fits Together

```
  operator ──▶ console / JSON API ──▶ iron-control ──▶ Postgres (encrypted secrets)
                                                             ▲
                                                         proxy-sync
                                                             ▲
                                           POST /api/v1/proxy/sync (iprx_ token)
                                                             │
  application ──▶ iron-proxy ────────────────────────────────┘
                     │
                     └──▶ upstream APIs (credentials added per request rules)
```

Operators manage credentials, principals, roles, and grants through the API or the console. Each `iron-proxy` instance signs in with its own token, fetches the configuration for its assigned principal, and adds the granted credentials to matching outbound requests.

## Console UI

The operator console is a React SPA in [`web/`](web/) (Vite, TypeScript,
shadcn/ui on Tailwind CSS, React Router, TanStack Query) that uses the same
JSON API. The login, pending-approval, MCP OAuth consent, and broker OAuth
result pages are server-rendered by api-rs, so installed MCP clients keep the
protocol behaviour they were built against and sign-in works without
JavaScript.

- **Serving.** `pnpm -C web build` writes the shell to `public/spa/index.html`
  and content-hashed assets to `public/spa/assets` (URLs under `/spa/`). api-rs
  serves the shell (`no-cache`, with the Content-Security-Policy) for `/` and
  every `/console/...` path, and React Router renders the page, including
  not-found. Nothing serves `/spa/index.html` directly. See
  `crates/centaur-api-server/src/console/static_files.rs`.
- **Data.** The SPA calls only the console API, `/api/v1/console/*`, with the
  console session cookie (`web/src/lib/api.ts` refuses any other path). It
  reads the session and nav flags from `GET /api/v1/console/me` and sends no
  CSRF token; api-rs checks an exact same-origin `Origin` on unsafe requests. A
  `401` sends the browser to `/login?return_to=<current page>` and a pending
  account to `/pending` (full page loads); sign-out is
  `DELETE /api/v1/console/session`. See
  [docs/API.md](docs/API.md#console-api).
- **Server routes**, answering with pages, redirects, or JSON:
  `/login`, `/logout`, `/pending`, `/auth/:provider/{start,callback}` (SSO),
  `/oauth/:slug/{start,callback}` (broker OAuth consent),
  `/mcp/oauth/{authorize,token,register}` and the `/.well-known` metadata (MCP
  OAuth). The server-rendered pages use plain CSS from
  `public/assets/oauth-page` (the SPA's colours, dark by default, light under
  `prefers-color-scheme: light`), embedded into api-rs, with no JavaScript. See
  [docs/API.md](docs/API.md#console-auth-pages).

## Local Development

Install Node.js with pnpm (the versions CI uses are in
`.github/workflows/console-ci.yml`). From the repository root:

```bash
pnpm install --frozen-lockfile --filter @centaur/console-web
pnpm -C services/console/web typecheck
pnpm -C services/console/web lint
pnpm -C services/console/web test
pnpm -C services/console/web build
```

To run the SPA against a live Console, start the local stack (`just up`) and
forward the Console Service:

```bash
kubectl port-forward -n <namespace> service/<release>-centaur-console 3000:3000
pnpm -C services/console/web dev
```

The Vite dev server (`http://localhost:5173`) proxies the API and server routes
to `CONSOLE_API_URL` (default `http://localhost:3000`) and keeps the browser's
`Host` header. api-rs selects the Console by host and checks writes against an
exact `Origin`, so set `console.publicUrl: http://localhost:5173` in your local
values while using the dev server. Sign in with the initial user's
`IRON_CONTROL_INITIAL_USER_EMAIL` / `IRON_CONTROL_INITIAL_USER_PASSWORD` from
the infra Secret.

The dev server sends no Content-Security-Policy; after a policy, dependency, or
`index.html` change, check the built SPA as api-rs serves it.

## Environment Variables

api-rs reads the Console settings below from `CENTAUR_CONSOLE_*` variables. The
chart mounts them from the shared infra Secret, where the older ones keep their
original `IRON_CONTROL_*` keys.

## First Boot

The console requires an authenticated user before any API endpoint will respond. To bootstrap a fresh deployment, set the following environment variables on api-rs:

| Variable                          | Required | Description                                                                                              |
| --------------------------------- | -------- | -------------------------------------------------------------------------------------------------------- |
| `CENTAUR_CONSOLE_INITIAL_USER_EMAIL`    | yes      | Email for the initial user.                                                                              |
| `CENTAUR_CONSOLE_INITIAL_USER_PASSWORD` | yes      | Password for the initial user (minimum 12 characters).                                                   |

Operators create API keys on the Console's API Keys page.

Behavior:

- Bootstrap runs at api-rs startup on every boot, but is a no-op if any user already exists. It is safe to leave the env vars set across rolling restarts.
- If `CENTAUR_CONSOLE_INITIAL_USER_EMAIL` is set without `CENTAUR_CONSOLE_INITIAL_USER_PASSWORD`, the process exits with a clear error.
- Concurrent pods racing the first boot are serialized with a Postgres advisory lock; exactly one user is created.

When deploying to Kubernetes, source these values from a `Secret`, not from a `ConfigMap`.

## Google, Slack, And Okta Authentication

The operator console supports email and password sign-in by default. To add Google, Slack, or Okta SSO buttons to the login page, configure an OAuth/OIDC client with the provider and set the matching client credentials in the environment. Okta also requires its issuer URL. A provider is shown only when all of its required settings are present.

| Variable                              | Required | Description                                                                                 |
| ------------------------------------- | -------- | ------------------------------------------------------------------------------------------- |
| `CENTAUR_CONSOLE_PUBLIC_URL`             | recommended | Public origin for this deployment, for example `https://control.example.com`. Required: api-rs selects the Console by this host and checks writes against this origin. |
| `CENTAUR_CONSOLE_GOOGLE_CLIENT_ID`       | for Google | Google OAuth client ID for console login.                                                    |
| `CENTAUR_CONSOLE_GOOGLE_CLIENT_SECRET`   | for Google | Google OAuth client secret for console login.                                                |
| `CENTAUR_CONSOLE_SLACK_CLIENT_ID`        | for Slack | Slack OpenID Connect client ID for console login.                                            |
| `CENTAUR_CONSOLE_SLACK_CLIENT_SECRET`    | for Slack | Slack OpenID Connect client secret for console login.                                        |
| `CENTAUR_CONSOLE_OKTA_ISSUER`            | for Okta  | Okta OIDC issuer URL, such as `https://id.example.com` or a custom authorization-server issuer. |
| `CENTAUR_CONSOLE_OKTA_CLIENT_ID`         | for Okta  | Okta OIDC web application client ID for console login.                                       |
| `CENTAUR_CONSOLE_OKTA_CLIENT_SECRET`     | for Okta  | Okta OIDC web application client secret for console login.                                   |
| `CENTAUR_CONSOLE_OKTA_TOKEN_ENDPOINT_AUTH_METHOD` | no | Okta token-endpoint client authentication method. Must match the app registration: `client_secret_basic` (default) or `client_secret_post`. |
| `CENTAUR_CONSOLE_SSO_EMAIL_DOMAINS`      | recommended for public exposure | Comma- or whitespace-separated domain allowlist for SSO users, for example `acme.com example.org`. Empty allows any IdP-authenticated email. |
| `CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED` | no       | Set to `false` to disable email and password sign-in. Defaults to enabled.                    |
| `CENTAUR_CONSOLE_BOOTSTRAP_ADMINS`       | no       | Comma- or whitespace-separated email allowlist. Matching users become active admins on first SSO login. Other accepted SSO users become active non-admin operators and land on the console directly. |

Register these callback URLs with the provider:

- Google: `<CENTAUR_CONSOLE_PUBLIC_URL>/auth/google/callback`
- Slack: `<CENTAUR_CONSOLE_PUBLIC_URL>/auth/slack/callback`
- Okta: `<CENTAUR_CONSOLE_PUBLIC_URL>/auth/okta/callback`

The Okta application must use the authorization-code flow with PKCE and the RS256 ID-token signing algorithm; request the `openid`, `email`, and `profile` scopes. The Console discovers the authorization, token, UserInfo, and JWKS endpoints from the issuer. It verifies each ID token's signature, issuer, audience, expiry, issued-at time, and nonce, then requires UserInfo to return the same subject and a verified email before signing a user in.

All three providers request the `openid`, `email`, and `profile` scopes.

Google OAuth consent apps for broker credentials are configured separately in the console under **OAuth Apps**. Those app callbacks use `/oauth/<slug>/callback` and currently support Google only; Slack support here applies to operator console sign-in.

## Company Context Syncs

The Console no longer runs Slack DM, Google Docs, or Granola sync jobs. Those
sources are ingested by [`centaur-company-context`](../../crates/centaur-company-context/),
which also stores the normalized documents and embeddings. Previously indexed
content is unchanged, and Google OAuth and broker-credential refresh still run
as before.

## Broker Credential Refresh

[`centaur-token-broker`](../token-broker/README.md) refreshes broker
credentials; the Console only creates and administers them.

## Encryption Keys

The Console encrypts secrets stored in the control plane (for example, the `control_plane` secret source type, SSH keys, and broker tokens) in the Active Record encryption format, which existing databases already hold. api-rs reads these keys; proxy-sync, token-broker, and company-context read the same values under their `IRON_CONTROL_AR_ENCRYPTION_*` names:

| Variable                                 | Required           | Description                                  |
| ---------------------------------------- | ------------------ | -------------------------------------------- |
| `CENTAUR_CONSOLE_AR_ENCRYPTION_PRIMARY_KEY`         | yes | Primary key used for non-deterministic encryption. |
| `CENTAUR_CONSOLE_AR_ENCRYPTION_KEY_DERIVATION_SALT` | yes | Salt used to derive per-attribute keys.      |

The chart mounts them from `IRON_CONTROL_AR_ENCRYPTION_PRIMARY_KEY` and `IRON_CONTROL_AR_ENCRYPTION_KEY_DERIVATION_SALT`, which local bootstrap generates when absent. A request that needs encryption fails when they are missing.

Rotating either key makes previously encrypted data unreadable. Treat them as long-lived secrets and back them up alongside other production credentials.

## API

The deployed Console API is the Rust one in
`crates/centaur-api-server/src/console/` (see its `README.md`): every credential
maps to one capability set and every route declares the capability it requires.
It exposes the JSON API under `/api/v1` in three namespaces:

- `/api/v1/*` is the operator API for machine callers. It takes an API key sent as a bearer token (`Authorization: Bearer iak_...`) whose owner is an active admin, and does not accept the console session cookie.
- `/api/v1/console/*` is the console API that the SPA calls. It takes the console session cookie, with an exact same-origin `Origin` check on writes (no CSRF token), or a sandbox-delegated user credential: a sandbox token from an active admin's Slack DM with the bot. It never accepts an API key (`403 session_required`). Endpoints are admin-only unless documented otherwise.
- `/api/v1/sandbox/*` takes the single sandbox token that `iron-proxy` injects for the plain per-sandbox resources.

`iron-proxy` instances fetch their configuration from the separate `proxy-sync` service with a proxy bearer token.

See [docs/API.md](docs/API.md) for the full reference: authentication, request/response conventions, pagination, error formats, the shared secret-source and request-rule shapes, and detailed payloads for every endpoint (static secrets, GCP auth secrets, OAuth token secrets, principals, roles, grants, API keys, proxies, and proxy sync), plus the [Console API](docs/API.md#console-api).

## Owner-only OAuth apps

An OAuth app with `owner_only` gives each Console user their own connection.
The credential is keyed by the connecting user, so the provider needs no
identity endpoint, and its token reaches only that user's own principals, in
DMs and, with `always_available`, in channels. It never reaches roles, other
principals, or email matches. Users can disconnect from Integrations; a
`custom` app's optional `revocation_endpoint` revokes the tokens at the
provider. The Runlayer preset sets one up for a tenant's MCP URL; see
[Runlayer setup](../../tools/infra/runlayer/README.md).
