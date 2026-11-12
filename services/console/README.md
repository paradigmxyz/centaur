# iron-control

`iron-control` is the control plane for [`iron-proxy`](https://github.com/ironsh/iron-proxy), a forwarding proxy that adds credentials to outbound HTTP requests. Applications route their traffic through `iron-proxy` and never hold the secrets themselves. `iron-control` stores the credentials, decides which proxy may use which credential on which requests, and hands each proxy its configuration.

It is a Rails application backed by Postgres. It provides a JSON API, an operator console, and encryption at rest for the secrets it stores.

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
result pages stay server-rendered by Rails, so installed MCP clients keep the
behaviour they were built against, sign-in works without JavaScript, and pods
on either side of a rolling deploy accept each other's forms.

- **Serving.** `pnpm -C web build` writes content-hashed assets to
  `public/spa` (URLs under `/spa/`) and the HTML shell to
  `spa-shell/index.html`. The shell is kept outside `public/` because the
  static file server would serve it without the Content-Security-Policy.
  `SpaController` returns it (`no-cache`) for any browser `GET`/`HEAD`
  navigation no Rails route claims, and React Router renders the page,
  including not-found. `/api`, `/rails`, `/spa`, `/assets`, `/.well-known`,
  `/up`, `/mcp`, `/oauth`, `/auth`, `/login`, `/logout`, and `/pending` never
  get the shell; unknown paths there return the JSON 404. See
  `lib/spa_request.rb` and `config/routes/spa.rb`.
- **Data.** The SPA uses only `/api/v1` with the console session cookie. It
  reads the session, CSRF token, nav flags, and one-shot flash from
  `GET /api/v1/me` and sends `X-CSRF-Token` on every write. A `401` sends the
  browser to `/login?return_to=<current page>` and a pending account to
  `/pending` (full page loads); sign-out is `DELETE /api/v1/session`. See
  [docs/API.md](docs/API.md#authentication).
- **Rails-owned browser routes**, answering with redirects or JSON:
  `/auth/:provider/{start,callback}` (SSO), `/oauth/:slug/{start,callback}`
  (broker OAuth consent), `/mcp/oauth/{authorize,token,register}` and the
  `/.well-known` metadata (MCP OAuth), `/launch` (PWA protocol handler), and
  `/manifest` / `/service-worker` (PWA files from `config/pwa`).
- **Server-rendered pages.** `GET /login` (SSO buttons and the password form),
  `POST /login`, and `DELETE /logout` (standard Rails forms with an
  authenticity token), `GET /pending`, the MCP consent page
  (`GET`/`POST /mcp/oauth/authorize`), and the failure page of
  `/oauth/:slug/{start,callback}`. After sign-in `/login` returns to the
  remembered page, such as the authorize URL with its query. See
  [docs/API.md](docs/API.md#console-auth-pages). They use
  `layouts/oauth.html.erb` and plain CSS from `public/oauth-page` (the SPA's
  colours, dark by default, light under `prefers-color-scheme: light`), with no
  asset pipeline or JavaScript.

## Local Development

Console runs on stock PostgreSQL 16, including managed services such as RDS
and Cloud SQL. Install Docker, Ruby using the version in `.ruby-version`,
Node.js with pnpm (the versions CI uses are in
`.github/workflows/console-ci.yml`), and Overmind or Foreman, then run:

```bash
just dev
```

This installs the gem and SPA dependencies, starts a persistent `postgres:16`
container named `centaur-console-postgres`, exposes it on `127.0.0.1:55432`,
prepares the Rails database, and starts Rails (`:3000`) and the Vite dev server
(`:5173`). Browse `http://localhost:5173`; Vite proxies the API and the
Rails-owned browser routes to Rails. Database files are
retained in the `centaur-console-postgres-data` Docker volume between runs.

To test the SPA as Rails serves it in production, run `just web-build` and
browse `http://localhost:3000`.

`just ci` (or `bin/ci`) runs the full local CI sequence: RuboCop, the gem
audit, Brakeman, the SPA's typecheck, lint, tests, and build, the Rails tests,
and seed validation.

To use an existing PostgreSQL server instead, set
`CENTAUR_CONSOLE_DB_HOST`, `CENTAUR_CONSOLE_DB_PORT`,
`CENTAUR_CONSOLE_DB_USERNAME`, and `CENTAUR_CONSOLE_DB_PASSWORD`, then set
`CENTAUR_CONSOLE_MANAGE_POSTGRES=false` so `just dev` does not start Docker.

## Environment Variables

All of the console's environment variables use the `CENTAUR_CONSOLE_` prefix. For backwards compatibility, every variable also resolves from the legacy `IRON_CONTROL_` name when the `CENTAUR_CONSOLE_` one is unset, so existing deployments keep working until they migrate. The `CENTAUR_CONSOLE_` name wins when both are set.

For local development, sign in through the normal login page at
`http://localhost:5173/login` (Vite proxies it to Rails) or
`http://localhost:3000/login` with the seeded initial user's
`CENTAUR_CONSOLE_INITIAL_USER_EMAIL` / `CENTAUR_CONSOLE_INITIAL_USER_PASSWORD`
credentials, the same as every other environment.

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

## Google, Slack, And Okta Authentication

The operator console supports email and password sign-in by default. To add Google, Slack, or Okta SSO buttons to the login page, configure an OAuth/OIDC client with the provider and set the matching client credentials in the environment. Okta also requires its issuer URL. A provider is shown only when all of its required settings are present.

| Variable                              | Required | Description                                                                                 |
| ------------------------------------- | -------- | ------------------------------------------------------------------------------------------- |
| `CENTAUR_CONSOLE_PUBLIC_URL`             | recommended | Public origin for this deployment, for example `https://control.example.com`. Use this when `iron-control` runs behind a proxy or load balancer whose internal host does not match the public URL. |
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

All three providers request the `openid`, `email`, and `profile` scopes. Client credentials may also be stored in Rails credentials under `console_auth.<provider>.client_id` and `console_auth.<provider>.client_secret`, but environment variables take precedence. Okta's issuer and token-endpoint authentication method may likewise be stored at `console_auth.okta.issuer` and `console_auth.okta.token_endpoint_auth_method`.

Google OAuth consent apps for broker credentials are configured separately in the console under **OAuth Apps**. Those app callbacks use `/oauth/<slug>/callback` and currently support Google only; Slack support here applies to operator console sign-in.

## Google Docs Sync

Google Docs metadata and document-content ETL is disabled by default. Set
`CENTAUR_CONSOLE_GOOGLE_DOCS_SYNC_ENABLED=true` to enable the recurring poller
and queued Google Docs sync jobs. This switch does not affect Google OAuth,
credential refresh, or previously indexed content.

## Company Context v1 Sync

`CENTAUR_CONSOLE_COMPANY_CONTEXT_V1_ENABLED=false` stops the Slack DM, Google
Docs, and Granola sync jobs, including jobs already queued, without deleting
synced data. It defaults to enabled; Google Docs sync also still requires
`CENTAUR_CONSOLE_GOOGLE_DOCS_SYNC_ENABLED`.

## Broker Credential Refresh

[`centaur-token-broker`](../token-broker/README.md) refreshes broker
credentials; the Console only creates and administers them.

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

`iron-control` exposes a JSON API under `/api/v1`. Each operator endpoint accepts an API key sent as a bearer token (`Authorization: Bearer iak_...`), the console session cookie, or either, as listed in [docs/API.md](docs/API.md#credential-types-by-endpoint). API keys and proxies are bearer-only. Cookie-authenticated writes also need the session's CSRF token. A bearer key works only while its owner is an active admin. Endpoints are admin-only unless documented otherwise. `iron-proxy` instances fetch their configuration from the separate `proxy-sync` service with a proxy bearer token.

See [docs/API.md](docs/API.md) for the full reference: authentication, request/response conventions, pagination, error formats, the shared secret-source and request-rule shapes, and detailed payloads for every endpoint (static secrets, GCP auth secrets, OAuth token secrets, principals, roles, grants, API keys, proxies, and proxy sync).
