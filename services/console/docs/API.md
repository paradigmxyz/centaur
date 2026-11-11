# iron-control API

`iron-control` exposes a JSON API under `/api/v1`. Resource endpoints require API key authentication or a console session cookie, and sandbox read endpoints use the sandbox entitlement JWT injected by `iron-proxy`.

- [Authentication](#authentication)
  - [Console session (browser SPA)](#console-session-browser-spa)
  - [Session endpoints](#session-endpoints)
  - [Console auth pages](#console-auth-pages)
- [Conventions](#conventions)
- [Errors](#errors)
- [Shared building blocks](#shared-building-blocks)
  - [Secret sources](#secret-sources)
  - [Request rules](#request-rules)
- [Static secrets](#static-secrets)
- [GCP auth secrets](#gcp-auth-secrets)
- [GCP ID token secrets](#gcp-id-token-secrets)
- [AWS auth secrets](#aws-auth-secrets)
- [OAuth token secrets](#oauth-token-secrets)
- [PG DSN secrets](#pg-dsn-secrets)
- [HMAC secrets](#hmac-secrets)
- [SSH key secrets](#ssh-key-secrets)
- [Broker credentials](#broker-credentials)
- [OAuth apps](#oauth-apps)
  - [OAuth providers](#oauth-providers)
  - [Integrations](#integrations)
- [OAuth consent flow](#oauth-consent-flow)
- [Principals](#principals)
- [Roles](#roles)
- [Grants](#grants)
- [Secrets (all kinds)](#secrets-all-kinds)
- [Slack channels](#slack-channels)
- [API keys](#api-keys)
- [Users](#users)
- [API clients](#api-clients)
- [System settings](#system-settings)
- [Scheduled tasks](#scheduled-tasks)
  - [Console scheduled tasks](#console-scheduled-tasks)
- [Workflows](#workflows)
- [Skills](#skills)
  - [Console skills](#console-skills)
- [Personal SSH key](#personal-ssh-key)
- [Proxies](#proxies)
- [Proxy sync](#proxy-sync)

## Authentication

Send your API key as a bearer token:

```
Authorization: Bearer iak_<64 lowercase hex chars>
```

API keys have the form `iak_` followed by 64 lowercase hex characters (a 32-byte hex string). The plaintext token is shown only once: when the key is created (or, for the bootstrap key, logged once at startup). Tokens are stored as SHA-256 hashes and cannot be recovered.

A missing or invalid token returns `401`:

```json
{ "error": { "message": "invalid or missing API key" } }
```

### Console session (browser SPA)

The console SPA calls the same `/api/v1` endpoints with the console session
cookie instead of a bearer token. An `Authorization` header, when present, is
authoritative: an invalid bearer token is rejected and never falls back to the
cookie.

- Cookie-authenticated requests other than `GET`/`HEAD` must send the
  session's CSRF token as `X-CSRF-Token` (from `GET /api/v1/me`). A missing or
  invalid token, or a cross-site `Origin`, returns `403` with
  `details.reason = "csrf"`. Bearer requests do not use CSRF tokens.
- A disabled account's session is cleared and the request returns `401` with
  `details.reason = "account_disabled"`.
- A pending account receives `403` with `details.reason = "account_pending"`,
  except on endpoints open to pending accounts (`GET /api/v1/me`,
  `DELETE /api/v1/session`).

### Authorization

Endpoints are admin-only unless documented otherwise. A non-admin caller
receives `403` with `details.reason = "admin_required"` (message
`"API key owner is not an admin"` for bearer keys, `"admin access required"`
for sessions). Endpoints documented as open to active users scope their records
to the caller unless the caller is an acting admin.

An admin session may pause its admin permissions ("view as operator"); while
descoped, admin-only endpoints return `403` for that session. Bearer keys are
unaffected.

### Session endpoints

| Method | Path | Access | Description |
|---|---|---|---|
| `GET` | `/api/v1/me` | session or bearer, including pending sessions | Caller identity and SPA navigation flags |
| `POST` | `/api/v1/descope` | admin session | Pause admin permissions for this session |
| `DELETE` | `/api/v1/descope` | active session | Restore admin permissions |
| `DELETE` | `/api/v1/session` | session (no-op for bearer) | Sign the console session out (`204`) |

`GET /api/v1/me` response (sent with `Cache-Control: no-store`):

```json
{
  "data": {
    "user": { "id": "usr_…", "email": "ops@example.com", "name": null, "status": "active", "admin": true },
    "auth_method": "session",
    "acting_admin": true,
    "descoped": false,
    "csrf_token": "…",
    "features": { "api_clients": false, "password_login": true },
    "flash": { "notice": "Signed in as ops@example.com." },
    "nav": {
      "control": true, "principals": true, "roles": true, "secrets": true,
      "credentials": true, "oauth_apps": true, "users": true, "api_clients": false,
      "settings": true, "workflows": true, "integrations": true, "skills": true,
      "scheduled_tasks": true
    }
  }
}
```

`csrf_token` is `null` for bearer callers. `flash` hands off (once) any
`notice`/`alert` left by a redirect-based flow such as SSO or an OAuth
callback; it is `{}` when there is none. `POST`/`DELETE /api/v1/descope`
return `{ "data": { "descoped": true|false } }`; bearer callers receive `422`.

### Console auth pages

Endpoints behind the SPA's signed-out and consent pages (`/login`, `/pending`,
`/oauth/result`, `/mcp/oauth/consent`).

| Method | Path | Access | Description |
|---|---|---|---|
| `GET` | `/api/v1/auth/config` | public (no session or key needed) | Login-page bootstrap: sign-in methods, CSRF token, one-shot flash |
| `POST` | `/api/v1/session` | public, CSRF token required | Password sign-in |
| `GET` | `/api/v1/mcp/authorization?request=` | active session (not bearer) | MCP consent ticket details |

`GET /api/v1/auth/config` (sent with `Cache-Control: no-store`; ignores any
`Authorization` header):

```json
{
  "data": {
    "providers": [ { "key": "google", "label": "Google", "start_path": "/auth/google/start" } ],
    "password_login": true,
    "csrf_token": "…",
    "signed_in": false,
    "redirect_to": null,
    "flash": { "alert": "Sign in was canceled." },
    "oauth_result": null
  }
}
```

- `providers` lists the configured SSO providers; `start_path` begins the
  browser redirect flow. `password_login` is `false` when
  `CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED=false`.
- `csrf_token` is bound to the (possibly new) session; send it as
  `X-CSRF-Token` to `POST /api/v1/session`.
- `signed_in` is `true` for an active or pending session (a disabled
  account's session is cleared). `redirect_to` is set only for an active
  session: where the login page should send it (`?return_to=` when
  allowlisted, else the landing page).
- `?return_to=` from a signed-out browser is remembered for the post-login
  redirect when it is an allowlisted same-origin path: `/`, `/console…`,
  `/mcp/oauth/authorize`, `/oauth/<slug>/start`, or `/launch` (no scheme,
  host, `//`, backslashes, control characters, or dot segments). Other values
  are ignored.
- `flash` hands off (once) the `notice`/`alert` left by a redirect: SSO
  failures, sign-out (`"Signed out."`), or a disabled account bounced by a
  Rails-owned route (`"Your account is disabled."`).
- `oauth_result` hands off (once) the outcome of a failed broker OAuth consent
  (`/oauth/<slug>/callback` and `/start` redirect to `/oauth/result`):
  `{ "kind": "error" | "denied", "message": "…", "retry_path": "/oauth/<slug>/start" }`.
  `retry_path` is omitted when the integration is unknown or disabled.

`POST /api/v1/session` body `{ "data": { "email": "…", "password": "…" } }`
(email is trimmed and matched case-insensitively). Success resets the session
(fixation) and returns `{ "data": { "status": "active" | "pending", "redirect_to": "/console/principals" } }`;
`redirect_to` is the remembered return path, the landing page, or `/pending`
for a pending account. The next `GET /api/v1/me` carries the `notice`. Errors:
`404` `password_login_disabled`, `422` `invalid_credentials` ("Invalid email
or password."), `422` `account_disabled`, `403` `csrf`.

`DELETE /api/v1/session` also leaves the `"Signed out."` notice for the login
page.

#### MCP OAuth consent

Dynamic client registration (`POST /mcp/oauth/register`) accepts `https`
redirect URIs, loopback `http`, and native private-use schemes (`cursor://`,
`vscode://`, `com.example.app:/…`). It rejects executable or browser-reserved
schemes (`javascript`, `data`, `vbscript`, `file`, `blob`, `about`, …, in any
case), and URIs containing whitespace, control characters, backslashes,
fragments, or wildcards. The same check runs again at authorize and decision
time, so stored registrations that predate it are refused.

`GET /mcp/oauth/authorize` validates the request (client, `response_type=code`,
registered `redirect_uri`, S256 PKCE, scopes, resource). Invalid requests go
back to the client's registered `redirect_uri` with `error`/`state`, or render a
plain `400` when the client or `redirect_uri` is not trusted. A valid request
redirects to `/mcp/oauth/consent?request=<ticket>`. The ticket is signed,
expires after 10 minutes, and is bound to the signed-in user and browser
session. It is also backed by a server-side row (`mcp_oauth_consents`) that a
decision consumes with one conditional `UPDATE`, so a ticket is decided at most
once, even when the session cookie is replayed or decisions race.

`GET /api/v1/mcp/authorization?request=<ticket>` (`Cache-Control: no-store`):

```json
{
  "data": {
    "client": { "id": "moc_…", "name": "Amp" },
    "redirect_host": "127.0.0.1",
    "resource": "https://mcp.example.com/mcp",
    "scopes": [ "mcp:tools" ],
    "user": { "email": "ops@example.com" },
    "decision_path": "/mcp/oauth/consent",
    "csrf_token": "…"
  }
}
```

Errors: `403` `session_required` for bearer callers, `404`
`invalid_consent_request` for a tampered, expired, decided, foreign-user, or
foreign-session ticket, and `422` `invalid_authorization_request` when the
client or its registration changed since the ticket was issued.

The decision is a classic form post (not JSON), so neither the client URL nor
the authorization code passes through page JavaScript:
`POST /mcp/oauth/consent` with `request=<ticket>`, `decision=approve|deny`, and
`authenticity_token`. The response is a `303` to the client's registered
`redirect_uri`, with `code` and `state` (or `error=access_denied`). A signed-out
browser goes to `/login` and a pending one to `/pending`. Failures redirect to
`/mcp/oauth/consent?error=<code>`, where `code` is one of `csrf`,
`invalid_decision` (the ticket is not consumed), `invalid_consent_request`,
`invalid_authorization_request`, or `server_error`. If the client's
redirect_uri is still trusted, a revalidation failure goes to the client
instead.

### Proxy tokens

`iron-proxy` instances authenticate to [proxy sync](#proxy-sync), served by the separate `proxy-sync` service, with their own token (`iprx_` followed by 64 lowercase hex characters), issued once when the proxy is created. An invalid proxy token returns `401` with `"invalid or missing proxy token"`.

Sandbox skill catalog endpoints use the existing sandbox entitlement JWT injected by `iron-proxy`.

## Conventions

### Namespace Compatibility

Resources are global and `foreign_id` values are globally unique within each resource type. The removed `namespace` and `credential_namespace` fields are not part of requests or responses.

Canonical foreign-id lookups use `/lookup/:foreign_id`. `/lookup/default/:foreign_id` remains as a temporary compatibility alias. Other formerly namespaced lookup paths return `404`.

- **Request bodies** wrap attributes in a top-level `data` object. A missing `data` key returns `400`.
- **Single-resource responses** wrap the resource in `data`.
- **List responses** include `data` (an array) and `meta` (pagination):

  ```json
  {
    "data": [ /* ... */ ],
    "meta": { "page": 1, "limit": 50, "total": 100, "total_pages": 2 }
  }
  ```

- **Pagination** uses the `page` (default `1`) and `limit` (default `50`, max `200`) query parameters. Values are clamped into range; a non-integer value returns `400`.
- **List filtering** accepts an optional `labels[key]=value` filter that matches by JSONB containment (all supplied pairs must be present). Label values must be scalars.
- **Object IDs** are prefixed by type: `ssr_` (static secret), `gas_` (GCP auth secret), `gid_` (GCP ID token secret), `ots_` (OAuth token secret), `prn_` (principal), `role_` (role), `grant_` (grant), `ak_` (API key), `prx_` (proxy).
- **`foreign_id`** must be URL-safe: only `A-Z a-z 0-9 - . _ ~`. It is optional for most resources and globally unique within its resource type. A `foreign_id` may not start with the resource's opaque-id prefix (e.g. `ssr_`), so it can never be mistaken for an OID.

### Upsert (`PUT` / `PATCH`)

For the resources with a `foreign_id` (static secrets, GCP auth secrets, GCP ID token secrets, OAuth token secrets, principals, roles), `PUT`/`PATCH /api/v1/<resource>/:id` is an **upsert**, and `:id` may be either an OID or a `foreign_id`:

- **`:id` is an OID** (it starts with the resource's prefix, e.g. `ssr_…`): updates that record. `404` if it does not exist — an OID is server-assigned, so it can't be created at a chosen value.
- **`:id` is anything else**: it is treated as a globally unique `foreign_id`. The record is **updated if it exists, created if it does not**. Creation responds `201`; update responds `200`.

This makes provisioning idempotent: `PUT /api/v1/roles/infra` converges the `infra` role whether or not it already exists, in one call. Omitting `foreign_id` from the body does not clear it.
- **`labels`** is an arbitrary string-keyed object (defaults to `{}`).
- **Timestamps** are ISO 8601 UTC.

## Errors

Errors return an `error` object with a `message` and, for validation failures, a `details` map of field name to messages:

```json
{
  "error": {
    "message": "validation failed",
    "details": {
      "base": ["must define one of inject_config or replace_config"],
      "name": ["can't be blank"]
    }
  }
}
```

| Status | Meaning                                                  |
| ------ | -------------------------------------------------------- |
| `200`  | OK                                                       |
| `201`  | Created                                                  |
| `204`  | No Content (successful `DELETE`)                         |
| `400`  | Bad Request (missing `data`, bad pagination/label query) |
| `401`  | Unauthorized (missing or invalid token)                 |
| `404`  | Not Found                                               |
| `422`  | Unprocessable Entity (validation failed)                |

## Shared building blocks

### Secret sources

A secret source describes where a credential value is resolved from. It appears as the `source` of a static secret, the `keyfile` of a GCP auth or GCP ID token secret, and each entry in an OAuth token secret's `credentials` and `token_endpoint_headers` maps.

Shape:

```json
{
  "source_type": "env",
  "config": { "var": "GITHUB_TOKEN" }
}
```

`source_type` is required and immutable. `config` is an object whose allowed keys depend on the type. Unknown keys are rejected. Every type additionally accepts the optional keys `json_key` (extract one field from a JSON value) and `ttl` (cache lifetime).

| `source_type`         | Required `config` keys | Type-specific optional keys | Notes |
| --------------------- | ---------------------- | --------------------------- | ----- |
| `env`                 | `var`                  | —                           | Reads a process environment variable. |
| `aws_sm`              | `secret_id`            | `region`                    | AWS Secrets Manager. |
| `aws_ssm`             | `name`                 | `region`, `with_decryption` | AWS SSM Parameter Store. |
| `1password`           | `secret_ref`           | `token_env`                 | 1Password CLI / service account. |
| `1password_connect`   | `secret_ref`           | `host_env`, `token_env`     | 1Password Connect server. |
| `control_plane`       | — (no config keys)     | —                           | Value is supplied inline; see below. |
| `token_broker`        | `credential_id`        | —                           | A managed [broker credential](#broker-credentials); see below. |

`control_plane` is special: the value is stored in iron-control itself. Supply it as a top-level `secret` field on the source (not inside `config`), and leave `config` empty:

```json
{
  "source_type": "control_plane",
  "secret": "the-actual-secret-value",
  "config": {}
}
```

The `secret` field is encrypted at rest, is write-only, and is never returned in any response. It is only permitted for `control_plane` sources; supplying it for any other type is a validation error, and omitting it for `control_plane` is also an error.

For the static, PG DSN, GCP auth, and GCP ID token secret resources, a `PATCH` whose `control_plane` source omits `secret` (or sends it blank) keeps the value already stored, provided the stored source is also `control_plane`. Clients can therefore save other edits without ever reading the plaintext back. `PUT` keeps its full-replace contract and still requires `secret`, as does a write that switches a source to `control_plane`.

On those same resources, an invalid nested source or rule is a `422` (it is never dropped silently or left half-written). `details` keeps the keys these responses have always used, and also adds a path-style key for every nested error: `source.config`, `dsn.secret`, `keyfile.source_type`, `rules[0].cidr` (0-based rule index), and so on.

- Static secrets: when the secret itself is valid, `details` carries the first invalid nested record's own keys, checking the source first and then the rules in order. For example, `{ "cidr": [...], "rules[1].cidr": [...] }`. Errors on the secret itself take precedence and come back alone, as before.
- PG DSN, GCP auth, and GCP ID token secrets: an invalid rule adds `"rules": ["is invalid"]`, and an invalid source adds `"dsn_source"` or `"keyfile_source"`: `["is invalid"]`. A create also reports the secret's own validation errors.

```json
{ "error": { "message": "validation failed", "details": {
  "rules": ["is invalid"],
  "rules[0].base": ["host and cidr are mutually exclusive"]
} } }
```

`token_broker` is also resolved by iron-control rather than by the proxy. `credential_id` names a [broker credential](#broker-credentials), and at sync time iron-control substitutes that credential's current access token, delivered inline exactly like a `control_plane` value. The reference never reaches the proxy. If the credential has no current token (it is still bootstrapping, or it is dead), the owning secret is omitted from the proxy's config until the credential recovers.

`credential_id` is either the credential's opaque id (`bcr_...`) or globally unique `foreign_id`. The reference is validated on write: it must resolve to an existing broker credential.

```json
{ "source_type": "token_broker", "config": { "credential_id": "bcr_abc123" } }
```

```json
{ "source_type": "token_broker", "config": { "credential_id": "gmail" } }
```

### Request rules

A rule scopes a credential to matching outbound requests. Rules appear as the `rules` array of static, GCP auth, GCP ID token, and OAuth secrets.

```json
{
  "host": "api.github.com",
  "http_methods": ["GET", "POST"],
  "paths": ["/repos/*"]
}
```

| Field          | Type             | Notes |
| -------------- | ---------------- | ----- |
| `host`         | string           | Hostname to match. Exactly one of `host` or `cidr` is required. |
| `cidr`         | string           | CIDR block to match (e.g. `10.0.0.0/8`). Must be a valid CIDR. |
| `http_methods` | array of strings | Each must be one of `GET`, `HEAD`, `POST`, `PUT`, `PATCH`, `DELETE`, `OPTIONS`, `CONNECT`, or `*`. |
| `paths`        | array of strings | Each must start with `/`. Glob patterns such as `/repos/*` are allowed. |

Rules are positional: a `position` (0-based, assigned from array order) is returned in responses but is not part of the request. On update, the supplied `rules` array fully replaces the existing rules.

## Static secrets

A static secret injects or replaces a fixed credential value on matching requests. It has a single secret [source](#secret-sources) and a list of [rules](#request-rules), and defines exactly one of `inject_config` or `replace_config`.

### Attributes

| Field            | In requests | Notes |
| ---------------- | ----------- | ----- |
| `foreign_id`     | optional    | Globally unique. Immutable after create. |
| `name`           | optional    | |
| `description`    | optional    | |
| `labels`         | optional    | Object; defaults to `{}`. |
| `inject_config`  | conditional | Define exactly one of `inject_config` / `replace_config`. |
| `replace_config` | conditional | |
| `source`         | optional    | A [secret source](#secret-sources). Replaced wholesale on update. |
| `rules`          | optional    | Array of [rules](#request-rules). Replaced wholesale on update. |

`inject_config` — inject the value into a request header or query parameter:

```json
{
  "header": "Authorization",       // exactly one of header / query_param
  "query_param": "api_key",
  "formatter": "Bearer {{ .Value }}"  // optional template
}
```

`replace_config` — replace an occurrence of a known placeholder in proxied traffic:

```json
{
  "proxy_value": "__GITHUB_TOKEN__",   // required, non-empty
  "match_headers": ["X-Token"],         // optional array of strings
  "match_body": true,                    // optional booleans
  "match_path": false,
  "match_query": false,
  "require": true
}
```

Both config objects reject unknown keys.

### Create

`POST /api/v1/static_secrets`

```json
{
  "data": {
    "foreign_id": "github-token",
    "name": "GitHub Token",
    "description": "Repo access",
    "labels": { "team": "platform" },
    "inject_config": { "header": "Authorization", "formatter": "Bearer {{ .Value }}" },
    "source": { "source_type": "env", "config": { "var": "GITHUB_TOKEN" } },
    "rules": [
      { "host": "api.github.com", "http_methods": ["GET", "POST"], "paths": ["/repos/*"] }
    ]
  }
}
```

Returns `201` with the created resource. Response shape:

```json
{
  "data": {
    "id": "ssr_...",
    "foreign_id": "github-token",
    "name": "GitHub Token",
    "description": "Repo access",
    "labels": { "team": "platform" },
    "inject_config": { "header": "Authorization", "formatter": "Bearer {{ .Value }}" },
    "replace_config": null,
    "source": { "source_type": "env", "config": { "var": "GITHUB_TOKEN" } },
    "rules": [
      { "host": "api.github.com", "cidr": null, "position": 0, "http_methods": ["GET", "POST"], "paths": ["/repos/*"] }
    ],
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

The `source` in responses never includes a `control_plane` `secret` value.

### Other operations

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/static_secrets` | List. `labels[k]=v` and pagination are optional. |
| `GET`  | `/api/v1/static_secrets/:id` | Fetch one. `404` if missing. |
| `GET`  | `/api/v1/static_secrets/lookup/:foreign_id` | Fetch by foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/static_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. `source` and `rules` are replaced wholesale. |
| `DELETE` | `/api/v1/static_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's source, rules, and any grants that reference it are removed. The granted roles and principals are not deleted. |

## GCP auth secrets

A GCP auth secret mints short-lived GCP OAuth2 access tokens and injects them as `Authorization: Bearer`. It defines exactly one credential mechanism: either a `keyfile` [secret source](#secret-sources) (the service account JSON) or a `credentials_provider` (Application Default Credentials).

### Attributes

| Field                  | In requests | Notes |
| ---------------------- | ----------- | ----- |
| `foreign_id`           | optional    | Globally unique. Immutable. |
| `name`, `description`  | optional    | |
| `labels`               | optional    | |
| `scopes`               | required    | Non-empty array of non-empty strings (GCP OAuth scopes). |
| `keyfile`              | conditional | A [secret source](#secret-sources). Define exactly one of `keyfile` / `credentials_provider`. |
| `credentials_provider` | conditional | Object `{ "type": "workload_identity" }`. Only `workload_identity` is accepted. |
| `subject`              | optional    | Email for domain-wide delegation. Only allowed with `keyfile`, not `credentials_provider`. |
| `rules`               | optional    | Array of [rules](#request-rules). |

### Create

`POST /api/v1/gcp_auth_secrets`

```json
{
  "data": {
    "foreign_id": "sa-prod",
    "name": "Production Service Account",
    "scopes": ["https://www.googleapis.com/auth/cloud-platform"],
    "subject": "user@example.com",
    "keyfile": {
      "source_type": "aws_sm",
      "config": { "secret_id": "gcp-sa-keyfile", "region": "us-west-2" }
    },
    "rules": [ { "host": "googleapis.com", "http_methods": ["*"], "paths": ["/v1/*"] } ]
  }
}
```

Or with workload identity instead of a keyfile:

```json
{
  "data": {
    "scopes": ["https://www.googleapis.com/auth/cloud-platform"],
    "credentials_provider": { "type": "workload_identity" },
    "rules": [ { "host": "googleapis.com", "http_methods": ["*"], "paths": ["/v1/*"] } ]
  }
}
```

Returns `201`. Response shape:

```json
{
  "data": {
    "id": "gas_...",
    "foreign_id": "sa-prod",
    "name": "Production Service Account",
    "description": null,
    "labels": {},
    "credentials_provider": null,
    "subject": "user@example.com",
    "scopes": ["https://www.googleapis.com/auth/cloud-platform"],
    "keyfile": { "source_type": "aws_sm", "config": { "secret_id": "gcp-sa-keyfile", "region": "us-west-2" } },
    "rules": [ { "host": "googleapis.com", "cidr": null, "position": 0, "http_methods": ["*"], "paths": ["/v1/*"] } ],
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

### Other operations

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/gcp_auth_secrets` | List. |
| `GET`  | `/api/v1/gcp_auth_secrets/:id` | Fetch one. |
| `GET`  | `/api/v1/gcp_auth_secrets/lookup/:foreign_id` | Fetch by foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/gcp_auth_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. |
| `DELETE` | `/api/v1/gcp_auth_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's sources, rules, and any grants that reference it are removed. The granted roles and principals are not deleted. |

## GCP ID token secrets

A GCP ID token secret mints Google-signed OIDC ID tokens for an audience and injects them as a bearer header. It is used for private Cloud Run services, Cloud Run functions, IAP, API Gateway, and other Google audience-authenticated targets. It requires a service-account `keyfile` [secret source](#secret-sources) and at least one [rule](#request-rules).

### Attributes

| Field                 | In requests | Notes |
| --------------------- | ----------- | ----- |
| `foreign_id`          | optional    | Globally unique. Immutable. |
| `name`, `description` | optional    | |
| `labels`              | optional    | Object; defaults to `{}`. |
| `audience`            | required    | ID token `aud` claim. For Cloud Run, use the service URL or configured custom audience. |
| `header`              | optional    | Omit or use `authorization` for `Authorization`; use `x-serverless-authorization` when the upstream app owns `Authorization`. |
| `keyfile`             | required    | A [secret source](#secret-sources) resolving the service account JSON. |
| `rules`               | required    | At least one [rule](#request-rules). |

### Create

`POST /api/v1/gcp_id_token_secrets`

```json
{
  "data": {
    "foreign_id": "cloud-run-caller",
    "name": "Cloud Run Caller",
    "audience": "https://my-service-abc123-uc.a.run.app",
    "header": "x-serverless-authorization",
    "keyfile": {
      "source_type": "1password_connect",
      "config": { "secret_ref": "op://Engineering/Cloud-Run-Caller/credential" }
    },
    "rules": [ { "host": "my-service-abc123-uc.a.run.app" } ]
  }
}
```

Returns `201`. Response shape:

```json
{
  "data": {
    "id": "gid_...",
    "foreign_id": "cloud-run-caller",
    "name": "Cloud Run Caller",
    "description": null,
    "labels": {},
    "audience": "https://my-service-abc123-uc.a.run.app",
    "header": "x-serverless-authorization",
    "keyfile": { "source_type": "1password_connect", "config": { "secret_ref": "op://Engineering/Cloud-Run-Caller/credential" } },
    "rules": [ { "host": "my-service-abc123-uc.a.run.app", "cidr": null, "position": 0, "http_methods": [], "paths": [] } ],
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

The `keyfile` in responses never includes a `control_plane` `secret` value.

### Other operations

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/gcp_id_token_secrets` | List. |
| `GET`  | `/api/v1/gcp_id_token_secrets/:id` | Fetch one. |
| `GET`  | `/api/v1/gcp_id_token_secrets/lookup/:foreign_id` | Fetch by foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/gcp_id_token_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. |
| `DELETE` | `/api/v1/gcp_id_token_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's source, rules, and any grants that reference it are removed. The granted roles and principals are not deleted. |

## AWS auth secrets

An AWS auth secret re-signs matching outbound requests with AWS SigV4. The workload's AWS SDK signs each request with throwaway placeholder credentials; `iron-proxy` strips that signature and re-signs with the real credentials, so the real keys never reach the workload. Each AWS credential is its own [secret source](#secret-sources): `access_key_id` and `secret_access_key` are required, and `session_token` is optional for temporary/STS credentials. `allowed_regions` and `allowed_services` scope what the proxy will sign for. At least one [rule](#request-rules) is required.

Each granted AWS auth secret is delivered to `iron-proxy` as its own `aws_auth` transform with its own rules (like a [GCP auth secret](#gcp-auth-secrets), and unlike OAuth token secrets, which are bundled).

### Attributes

| Field               | In requests | Notes |
| ------------------- | ----------- | ----- |
| `foreign_id`        | optional    | Globally unique. Immutable. |
| `name`, `description` | optional  | |
| `labels`            | optional    | Object; defaults to `{}`. |
| `allowed_regions`   | optional    | Array of non-empty strings; defaults to `[]` (no region scoping). |
| `allowed_services`  | optional    | Array of non-empty strings; defaults to `[]` (no service scoping). |
| `access_key_id`     | required    | A [secret source](#secret-sources) resolving the AWS access key id. |
| `secret_access_key` | required    | A [secret source](#secret-sources) resolving the AWS secret access key. |
| `session_token`     | optional    | A [secret source](#secret-sources) resolving an STS session token, for temporary credentials. |
| `rules`             | required    | At least one [rule](#request-rules). |

### Create

`POST /api/v1/aws_auth_secrets`

```json
{
  "data": {
    "foreign_id": "cloudwatch-reader",
    "name": "CloudWatch Reader",
    "allowed_regions": ["us-west-2"],
    "allowed_services": ["logs", "monitoring"],
    "access_key_id": { "source_type": "aws_sm", "config": { "secret_id": "aws-access-key-id", "region": "us-west-2" } },
    "secret_access_key": { "source_type": "aws_sm", "config": { "secret_id": "aws-secret-access-key", "region": "us-west-2" } },
    "rules": [ { "host": "logs.us-west-2.amazonaws.com", "http_methods": ["POST"] } ]
  }
}
```

For temporary/STS credentials, include a `session_token` source as well:

```json
{
  "data": {
    "foreign_id": "cloudwatch-reader-sts",
    "access_key_id": { "source_type": "env", "config": { "var": "AWS_ACCESS_KEY_ID" } },
    "secret_access_key": { "source_type": "env", "config": { "var": "AWS_SECRET_ACCESS_KEY" } },
    "session_token": { "source_type": "env", "config": { "var": "AWS_SESSION_TOKEN" } },
    "rules": [ { "host": "logs.us-west-2.amazonaws.com", "http_methods": ["POST"] } ]
  }
}
```

Returns `201`. Response shape (each credential echoes its source as `{ source_type, config }`, never the underlying value; `session_token` is `null` when unset):

```json
{
  "data": {
    "id": "aas_...",
    "foreign_id": "cloudwatch-reader",
    "name": "CloudWatch Reader",
    "description": null,
    "labels": {},
    "allowed_regions": ["us-west-2"],
    "allowed_services": ["logs", "monitoring"],
    "access_key_id": { "source_type": "aws_sm", "config": { "secret_id": "aws-access-key-id", "region": "us-west-2" } },
    "secret_access_key": { "source_type": "aws_sm", "config": { "secret_id": "aws-secret-access-key", "region": "us-west-2" } },
    "session_token": null,
    "rules": [ { "host": "logs.us-west-2.amazonaws.com", "cidr": null, "position": 0, "http_methods": ["POST"], "paths": [] } ],
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

### Other operations

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/aws_auth_secrets` | List. |
| `GET`  | `/api/v1/aws_auth_secrets/:id` | Fetch one. |
| `GET`  | `/api/v1/aws_auth_secrets/lookup/:foreign_id` | Fetch by foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/aws_auth_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. Replaces the credential sources wholesale, so a `PUT` must resend `access_key_id` and `secret_access_key` (and `session_token`, to keep it). |
| `DELETE` | `/api/v1/aws_auth_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's sources, rules, and any grants that reference it are removed. The granted roles and principals are not deleted. |

## OAuth token secrets

An OAuth token secret mints OAuth2 access tokens for a single grant and injects them as a bearer header. Each credential field and each token-endpoint header is its own [secret source](#secret-sources). At least one [rule](#request-rules) is required.

### Attributes

| Field                    | In requests | Notes |
| ------------------------ | ----------- | ----- |
| `foreign_id`             | optional    | Globally unique. Immutable. |
| `name`, `description`    | optional    | |
| `labels`                 | optional    | |
| `grant`                  | required    | One of `refresh_token`, `client_credentials`, `password`, `jwt_bearer`. |
| `token_endpoint`         | required    | Token endpoint URL. |
| `audience`               | conditional | Required when `grant` is `jwt_bearer`; otherwise optional. |
| `scopes`                 | optional    | Array of strings. |
| `header`                 | optional    | Header to inject the token into. |
| `value_prefix`           | optional    | Prefix for the injected value (e.g. `Bearer`). |
| `credentials`            | required    | Object mapping credential field → [secret source](#secret-sources). Required/allowed fields depend on `grant` (see below). |
| `token_endpoint_headers` | optional    | Object mapping header name → [secret source](#secret-sources). |
| `rules`                  | required    | At least one [rule](#request-rules). |

Credential fields per grant:

| `grant`              | Required credential fields           | Optional credential fields |
| -------------------- | ------------------------------------ | -------------------------- |
| `refresh_token`      | `refresh_token`, `client_id`         | `client_secret`            |
| `client_credentials` | `client_id`, `client_secret`         | —                          |
| `password`           | `username`, `password`, `client_id`  | `client_secret`            |
| `jwt_bearer`         | `issuer`, `subject`, `private_key`   | `private_key_id`           |

Supplying a credential field that the chosen grant does not use, or omitting a required one, is a validation error.

### Create

`POST /api/v1/oauth_token_secrets`

```json
{
  "data": {
    "foreign_id": "slack-app",
    "name": "Slack App OAuth",
    "grant": "refresh_token",
    "token_endpoint": "https://slack.com/api/oauth.v2.access",
    "scopes": ["chat:write"],
    "header": "Authorization",
    "value_prefix": "Bearer",
    "credentials": {
      "client_id": { "source_type": "aws_ssm", "config": { "name": "/slack/client_id" } },
      "client_secret": { "source_type": "aws_ssm", "config": { "name": "/slack/client_secret", "with_decryption": true } },
      "refresh_token": { "source_type": "control_plane", "secret": "xoxe-1-...", "config": {} }
    },
    "token_endpoint_headers": {
      "X-Auth": { "source_type": "env", "config": { "var": "SLACK_AUTH_HEADER" } }
    },
    "rules": [ { "host": "slack.com", "http_methods": ["POST"], "paths": ["/api/*"] } ]
  }
}
```

Returns `201`. Response shape (note that `credentials` and `token_endpoint_headers` echo each source as `{ source_type, config }`, never the underlying `secret`):

```json
{
  "data": {
    "id": "ots_...",
    "foreign_id": "slack-app",
    "name": "Slack App OAuth",
    "description": null,
    "labels": {},
    "grant": "refresh_token",
    "token_endpoint": "https://slack.com/api/oauth.v2.access",
    "audience": null,
    "scopes": ["chat:write"],
    "header": "Authorization",
    "value_prefix": "Bearer",
    "credentials": {
      "client_id": { "source_type": "aws_ssm", "config": { "name": "/slack/client_id" } },
      "client_secret": { "source_type": "aws_ssm", "config": { "name": "/slack/client_secret", "with_decryption": true } },
      "refresh_token": { "source_type": "control_plane", "config": {} }
    },
    "token_endpoint_headers": {
      "X-Auth": { "source_type": "env", "config": { "var": "SLACK_AUTH_HEADER" } }
    },
    "rules": [ { "host": "slack.com", "cidr": null, "position": 0, "http_methods": ["POST"], "paths": ["/api/*"] } ],
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

### Other operations

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/oauth_token_secrets` | List. |
| `GET`  | `/api/v1/oauth_token_secrets/:id` | Fetch one. |
| `GET`  | `/api/v1/oauth_token_secrets/lookup/:foreign_id` | Fetch by foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/oauth_token_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. |
| `DELETE` | `/api/v1/oauth_token_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's sources, rules, and any grants that reference it are removed. The granted roles and principals are not deleted. |

## PG DSN secrets

A PG DSN secret is a Postgres upstream credential: a connection string (DSN) resolved from a single secret [source](#secret-sources), plus an optional `SET ROLE` for the upstream session. It is delivered to `iron-proxy` with a required `foreign_id` and a required `database` for routing. The proxy multiplexes upstreams through one listener and routes by the Postgres database name the client sends. Multiple secrets may use the same `database` so different principals can route that database through different upstream roles. A principal's effective proxy config emits only one route per database, with higher-priority grants winning.

Listener and client knobs (bind address, client auth) are deliberately not modeled: they are proxy-host deployment concerns. There are no [request rules](#request-rules) either: a Postgres listener matches by port, not by request.

### Attributes

| Field         | In requests | Notes |
| ------------- | ----------- | ----- |
| `foreign_id`  | required    | Globally unique. Immutable after create. |
| `name`        | optional    | |
| `description` | optional    | |
| `labels`      | optional    | Object; defaults to `{}`. |
| `database`    | required    | Database name clients connect to through the proxy. Must match the upstream DSN's database. If several granted secrets use the same database, grant priority selects the effective route. |
| `role`        | optional    | Upstream `SET ROLE` applied to the session. |
| `settings`    | optional    | Ordered array of session variables (GUCs) the proxy SETs at session start, before the `SET ROLE`, and pins so clients cannot override them. Each entry is `{ "name", "value" }` for a literal value, or `{ "name", "value_from" }` to resolve the value from the assigned proxy principal or proxy labels at sync time (see [derived setting values](#derived-setting-values)). Names must be a bare or dotted identifier; `role` and `session_authorization` are reserved. Replaced wholesale on update. |
| `dsn`         | required    | A [secret source](#secret-sources) resolving to the connection string. Replaced wholesale on update. |

### Create

`POST /api/v1/pg_dsn_secrets`

```json
{
  "data": {
    "foreign_id": "analytics-pg",
    "name": "Analytics DB",
    "description": "Read-only reporting",
    "labels": { "team": "data" },
    "database": "analytics",
    "role": "readonly",
    "settings": [ { "name": "app.tenant", "value": "centaur" } ],
    "dsn": { "source_type": "env", "config": { "var": "PG_ANALYTICS_DSN" } }
  }
}
```

Returns `201` with the created resource. Response shape:

```json
{
  "data": {
    "id": "pgs_...",
    "foreign_id": "analytics-pg",
    "name": "Analytics DB",
    "description": "Read-only reporting",
    "labels": { "team": "data" },
    "database": "analytics",
    "role": "readonly",
    "settings": [ { "name": "app.tenant", "value": "centaur" } ],
    "dsn": { "source_type": "env", "config": { "var": "PG_ANALYTICS_DSN" } },
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

The `dsn` in responses never includes a `control_plane` `secret` value.

### Derived setting values

A setting may take its value from the proxy's assigned principal or proxy
labels instead of storing a literal, by replacing `value` with `value_from`:

```json
{ "name": "centaur.slack_channel_id", "value_from": { "principal_field": "slack_channel_id" } }
```

`value_from` contains exactly one of:

| Key               | Resolves to |
| ----------------- | ----------- |
| `principal_label` | The named label on the assigned principal. A label the principal does not carry resolves to an empty string, so RLS-style policies fail closed. |
| `principal_field` | One of the principal's fields: `id` (the opaque `prn_...` id), `foreign_id`, `name`, `kind`, `slack_user_id`, `slack_channel_id`, `slack_team_id`, `slack_email`, `console_user_id`, `console_user_email` (the associated Console user's current email), or `slack_history_channel_ids` (JSON array of Slack channel IDs with history permission). |
| `proxy_label`     | The named label on the proxy. A label the proxy does not carry resolves to an empty string, so RLS-style policies fail closed. |

A setting has either `value` or `value_from`, never both; unknown
`principal_field` names and blank label keys are rejected at create
and update time. References are resolved only in the proxy sync and
effective-config payloads; create, update, show, and list responses echo the
stored reference.

### Other operations

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/pg_dsn_secrets` | List. `labels[k]=v` and pagination are optional. |
| `GET`  | `/api/v1/pg_dsn_secrets/:id` | Fetch one. `404` if missing. |
| `GET`  | `/api/v1/pg_dsn_secrets/lookup/:foreign_id` | Fetch by foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/pg_dsn_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. `dsn` is replaced wholesale. |
| `DELETE` | `/api/v1/pg_dsn_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's source and any grants that reference it are removed. The granted roles and principals are not deleted. |

## HMAC secrets

An HMAC secret signs matching outbound requests with an HMAC over a templated message and injects the signature (and any companion values) as request headers. The HMAC key is one [secret source](#secret-sources) under the required `secret` credential; additional named credentials are optional and available to the message and header templates as `.Credentials.<name>`. At least one [rule](#request-rules) is required.

Each granted HMAC secret is delivered to `iron-proxy` as its own `hmac_sign` transform with its own rules (like a [GCP auth secret](#gcp-auth-secrets), and unlike OAuth token secrets, which are bundled).

### Attributes

| Field                       | In requests | Notes |
| --------------------------- | ----------- | ----- |
| `foreign_id`                | optional    | Globally unique. Immutable. |
| `name`, `description`       | optional    | |
| `labels`                    | optional    | Object; defaults to `{}`. |
| `timestamp_format`          | required    | One of `unix_seconds`, `unix_millis`, `unix_nanos`, `rfc3339`. |
| `signature_algorithm`       | required    | One of `sha256`, `sha512`, `sha1`. |
| `signature_key_encoding`    | required    | How the key bytes are encoded: one of `raw`, `base64`, `hex`. |
| `signature_output_encoding` | required    | How the signature is encoded: one of `base64`, `hex`. |
| `signature_message`         | required    | Template for the signed message. Has access to `.Timestamp`, `.Body`, `.Credentials.<name>`, etc. |
| `allow_chunked_body`        | optional    | Defaults to `false`. Allow signing requests with a chunked body. |
| `headers`                   | required    | Non-empty array of `{ "name", "value" }` injected headers; values are templates (e.g. `{{ .Signature }}`). |
| `credentials`               | required    | Object mapping credential name → [secret source](#secret-sources). Must include `secret` (the HMAC key); other names are optional. |
| `rules`                     | required    | At least one [rule](#request-rules). |

### Create

`POST /api/v1/hmac_secrets`

```json
{
  "data": {
    "foreign_id": "webhook-hmac",
    "name": "Webhook Signing",
    "timestamp_format": "unix_seconds",
    "signature_algorithm": "sha256",
    "signature_key_encoding": "hex",
    "signature_output_encoding": "base64",
    "signature_message": "{{ .Timestamp }}.{{ .Body }}",
    "headers": [
      { "name": "X-Signature", "value": "{{ .Signature }}" },
      { "name": "X-Timestamp", "value": "{{ .Timestamp }}" }
    ],
    "credentials": {
      "secret": { "source_type": "aws_sm", "config": { "secret_id": "webhook-hmac-key", "region": "us-west-2" } }
    },
    "rules": [ { "host": "hooks.example.com", "http_methods": ["POST"], "paths": ["/webhooks/*"] } ]
  }
}
```

Returns `201`. Response shape (note that `credentials` echoes each source as `{ source_type, config }`, never the underlying `secret`):

```json
{
  "data": {
    "id": "hms_...",
    "foreign_id": "webhook-hmac",
    "name": "Webhook Signing",
    "description": null,
    "labels": {},
    "timestamp_format": "unix_seconds",
    "signature_algorithm": "sha256",
    "signature_key_encoding": "hex",
    "signature_output_encoding": "base64",
    "signature_message": "{{ .Timestamp }}.{{ .Body }}",
    "allow_chunked_body": false,
    "headers": [
      { "name": "X-Signature", "value": "{{ .Signature }}" },
      { "name": "X-Timestamp", "value": "{{ .Timestamp }}" }
    ],
    "credentials": {
      "secret": { "source_type": "aws_sm", "config": { "secret_id": "webhook-hmac-key", "region": "us-west-2" } }
    },
    "rules": [ { "host": "hooks.example.com", "cidr": null, "position": 0, "http_methods": ["POST"], "paths": ["/webhooks/*"] } ],
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

### Other operations

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/hmac_secrets` | List. |
| `GET`  | `/api/v1/hmac_secrets/:id` | Fetch one. |
| `GET`  | `/api/v1/hmac_secrets/lookup/:foreign_id` | Fetch by foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/hmac_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. |
| `DELETE` | `/api/v1/hmac_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's sources, rules, and any grants that reference it are removed. The granted roles and principals are not deleted. |

## SSH key secrets

An SSH key secret is a private key that `iron-proxy` presents to a deployment-allowed SSH gateway on a sandbox's behalf. It is granted like any other secret, but it is never part of [proxy sync](#proxy-sync): proxy-sync's `POST /api/v1/proxy/ssh/authorize` releases the strongest granted key for the proxy's assigned principal, one connection at a time, only for a destination allowed by the deployment's SSH target policy. Two keys granted at the same top priority are ambiguous and deny access.

The private key is one [secret source](#secret-sources). A `control_plane` key must be an unencrypted Ed25519, RSA, or ECDSA private key; its public key is derived on save. Other source types resolve on the proxy host, so `public_key` is required for them.

A personal key (`personal: true`) belongs to a Console user, who manages it from **Integrations → SSH key**. Saving it grants it to that user's `console_user` principal and to the `slack_dm` principal of their single verified Slack identity. It reaches any other principal or role only through an explicit grant. The API can read and delete personal keys but cannot update them (`403`).

### Attributes

| Field                   | In requests | Notes |
| ----------------------- | ----------- | ----- |
| `foreign_id`            | optional    | Globally unique. Immutable. |
| `name`                  | required    | At most 100 characters. |
| `description`           | optional    | |
| `labels`                | optional    | Object; defaults to `{}`. |
| `private_key`           | required    | [Secret source](#secret-sources). Echoed as `{ source_type, config }`, never its value. |
| `public_key`            | conditional | OpenSSH public key. Derived for `control_plane` sources; required otherwise. |
| `personal`              | read-only   | Whether a Console user owns the key. |

### Create

`POST /api/v1/ssh_key_secrets`

```json
{
  "data": {
    "foreign_id": "team-dev-ssh",
    "name": "Team development key",
    "public_key": "ssh-ed25519 AAAA...",
    "private_key": { "source_type": "env", "config": { "var": "TEAM_DEV_SSH_KEY" } }
  }
}
```

Returns `201` with the document, then grant it with [`POST /api/v1/grants`](#grants) and `ssh_key_secret_id`.

### Other operations

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/ssh_key_secrets` | List. `labels[k]=v` and pagination are optional. |
| `GET`  | `/api/v1/ssh_key_secrets/:id` | Fetch one. |
| `GET`  | `/api/v1/ssh_key_secrets/lookup/:foreign_id` | Fetch by foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/ssh_key_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. `403` for a personal key. |
| `DELETE` | `/api/v1/ssh_key_secrets/:id` | Delete. Returns `204`. Cascades to the key's source and every grant of it; active connections close at the next authorization check. |

## Broker credentials

A broker credential is an OAuth credential whose token lifecycle iron-control manages itself. iron-control runs the refresh loop, mints fresh access tokens before they expire, and delivers the current access token to `iron-proxy` inline through [proxy sync](#proxy-sync) wherever a [`token_broker` secret source](#secret-sources) references the credential by its `id`.

Unlike the secret types above, a broker credential is not granted directly and is not injected on its own. It is referenced by a `token_broker` source on a grantable secret (typically a [static secret](#static-secrets)), which carries the rules and injection config. Refresh tokens, usernames, passwords, and API keys never leave iron-control.

The token credentials it refreshes with are fields on the credential, resolved by iron-control itself. `client_id` is not secret and is returned in responses; `client_secret`, password-grant fields, Preqin API keys, and the `token_endpoint_headers` values are encrypted at rest and never returned.

### Attributes

| Field                          | In requests | Notes |
| ------------------------------ | ----------- | ----- |
| `foreign_id`                   | optional    | Globally unique. Immutable. |
| `name`, `description`          | optional    | |
| `labels`                       | optional    | |
| `grant`                        | optional    | One of `refresh_token`, `client_credentials`, `password`, or `preqin`. Defaults to `refresh_token`. |
| `token_endpoint`               | conditional | Token endpoint the refresh request is sent to. Required except `preqin`, which uses the fixed `https://api.preqin.com/connect/token` endpoint. |
| `scopes`                       | optional    | Array of strings. |
| `client_id`                    | conditional | OAuth client id. Required for standalone `refresh_token`, `client_credentials`, and `password` credentials. Returned in responses. Not used for `preqin`. |
| `client_secret`                | conditional | OAuth client secret. Required for `client_credentials`, optional for `refresh_token` and `password`, and not used for `preqin`. Write-only and encrypted at rest; omit for public clients. Never returned. |
| `token_endpoint_headers`       | optional    | Object mapping header name to a string value, sent on the refresh request. Values are write-only and encrypted; only the header names are returned (as `token_endpoint_header_names`). When supplied, the object replaces the stored headers, except that a `null` value for a header name that is already stored keeps that header's stored value, so a client can keep, drop, or add headers without re-entering existing values. A `null` value for a new name is rejected with `422`; `null` for the whole field clears the headers. |
| `refresh_token`                | optional    | Write-only initial value for `refresh_token` credentials. Also used by `password` credentials when the provider returns one. Not used by `client_credentials` credentials. Supplying a value schedules the credential immediately and clears dead state. Never returned. |
| `username`                     | conditional | Required for `password` credentials. Write-only and encrypted at rest. Never returned. |
| `password`                     | conditional | Required for `password` credentials. Write-only and encrypted at rest. Never returned. |
| `api_key`                      | conditional | Required for `preqin` credentials. Write-only and encrypted at rest. Never returned. |
| `early_refresh_slack_seconds`  | optional    | Refresh this many seconds before expiry. Defaults to `300`. |
| `early_refresh_fraction`       | optional    | Refresh once this fraction of the token's lifetime remains, when that is larger than the slack. In `[0, 1)`. Defaults to `0.2`. |
| `max_refresh_interval_seconds` | optional    | Refresh at least this often, even for long-lived tokens. Defaults to `86400`. |
| `refresh_timeout_seconds`      | optional    | Per-attempt timeout for the token endpoint request. Defaults to `30`. |

Read-only fields are returned but never accepted in requests:

| Field                         | Notes |
| ----------------------------- | ----- |
| `status`                      | `bootstrapping` (no token minted yet), `live`, or `dead` (an unrecoverable refresh failure; needs fresh initial values). |
| `token_endpoint_header_names` | The configured header names (values are not returned). |
| `expires_at`                  | When the current access token expires. |
| `last_refresh`                | When the last successful refresh completed. |
| `next_attempt_at`             | When the next refresh is scheduled. |
| `dead`                        | Whether the credential is dead. |
| `dead_reason`                 | Why it is dead (e.g. `invalid_grant`). |
| `failure_count`               | Consecutive retryable failures since the last success. |
| `oauth_app_id`                | The [OAuth app](#oauth-apps) that minted this credential through the consent flow, or `null` for a standalone credential. |
| `provider_subject`            | The IdP-stable account id (Google `sub`) for a flow-minted credential. |
| `provider_email`              | The account email captured at consent time. |
| `external_user_key`           | An opaque key generated for the credential when it is minted by the consent flow. |
| `oauth_app`                   | `{ id, slug, provider }` for the minting [OAuth app](#oauth-apps), or `null`. |
| `effective_client_id`         | The client id the refresh loop uses: the OAuth app's for a flow-minted credential, otherwise `client_id`. |
| `has_client_secret`, `has_refresh_token`, `has_username`, `has_password`, `has_api_key` | Whether the credential stores that write-only value. The values themselves are never returned. |
| `static_secret`               | `{ id, foreign_id, name }` for the grantable [static secret](#static-secrets) that wraps this credential, or `null`. |

The minted `access_token`, the `refresh_token`, `username`, `password`, `api_key`, the `client_secret`, and the `token_endpoint_headers` values are never returned in any response.

Password-grant credentials first use the stored initial values with `grant_type=password`. If the token endpoint returns a `refresh_token`, iron-control stores it and uses `grant_type=refresh_token` on later scheduled refreshes. If that stored refresh token is rejected with an unrecoverable OAuth error, iron-control retries once with `grant_type=password`; retryable network, 5xx, rate-limit, and parse failures keep the existing backoff behavior and do not fall back to password.

Client-credentials credentials use the stored `client_id` and encrypted `client_secret` with `grant_type=client_credentials` every time they mint an access token. Providers that return only `access_token`, `token_type`, and `expires_in` are supported; no `refresh_token` is required or stored.

Credentials minted by the [OAuth consent flow](#oauth-consent-flow) are linked to an OAuth app and delegate their `client_id` and `client_secret` to it: rotating the app's secret applies to every credential it minted. Such a credential needs no `client_id`/`client_secret` of its own, and its `scopes` reflect exactly what the IdP granted.

### Create

`POST /api/v1/broker_credentials`

```json
{
  "data": {
    "foreign_id": "gmail",
    "name": "Gmail",
    "grant": "refresh_token",
    "token_endpoint": "https://oauth2.googleapis.com/token",
    "scopes": ["https://www.googleapis.com/auth/gmail.readonly"],
    "client_id": "1234.apps.googleusercontent.com",
    "client_secret": "GOCSPX-...",
    "refresh_token": "1//0g..."
  }
}
```

Returns `201`. The token blob, the `refresh_token` seed, and the `client_secret` are never echoed back:

```json
{
  "data": {
    "id": "bcr_...",
    "foreign_id": "gmail",
    "name": "Gmail",
    "description": null,
    "labels": {},
    "grant": "refresh_token",
    "token_endpoint": "https://oauth2.googleapis.com/token",
    "scopes": ["https://www.googleapis.com/auth/gmail.readonly"],
    "client_id": "1234.apps.googleusercontent.com",
    "token_endpoint_header_names": [],
    "early_refresh_slack_seconds": 300,
    "early_refresh_fraction": 0.2,
    "max_refresh_interval_seconds": 86400,
    "refresh_timeout_seconds": 30,
    "status": "bootstrapping",
    "expires_at": null,
    "last_refresh": null,
    "next_attempt_at": "2026-06-01T10:00:00Z",
    "dead": false,
    "dead_reason": null,
    "failure_count": 0,
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

Password-grant providers use the same endpoint with `grant: "password"`:

```json
{
  "data": {
    "foreign_id": "password-provider",
    "name": "Password Provider",
    "grant": "password",
    "token_endpoint": "https://auth.example.com/token",
    "client_id": "client-id",
    "client_secret": "client-secret",
    "username": "user@example.com",
    "password": "account-password",
    "token_endpoint_headers": { "x-api-key": "api-key" }
  }
}
```

Client-credentials providers use the same endpoint with `grant: "client_credentials"`:

```json
{
  "data": {
    "foreign_id": "bloomberg-dl",
    "name": "Bloomberg DL",
    "grant": "client_credentials",
    "token_endpoint": "https://bsso.blpprofessional.com/ext/api/as/token.oauth2",
    "client_id": "client-id",
    "client_secret": "client-secret"
  }
}
```

For API calls that require static headers alongside the broker token, grant static secrets with the broker credential so the proxy also injects headers such as `x-api-key` and `clientid`. The broker credential itself only supplies the current bearer token through a `token_broker` source.

Preqin Operational API credentials use the provider-specific `preqin` grant. iron-control submits Preqin's multipart `username` and `apikey` form, stores any returned `refresh_token`, and later uses Preqin's refresh endpoint when possible:

```json
{
  "data": {
    "foreign_id": "preqin-operational",
    "name": "Preqin Operational",
    "grant": "preqin",
    "username": "preqinapiuser@example.com",
    "api_key": "preqin-api-key"
  }
}
```

The token is still consumed like any other broker token:

```json
{
  "data": {
    "foreign_id": "preqin-operational-auth",
    "inject_config": { "header": "Authorization", "formatter": "Bearer {{ .Value }}" },
    "source": {
      "source_type": "token_broker",
      "config": { "credential_id": "preqin-operational" }
    },
    "rules": [ { "host": "api.preqin.com" } ]
  }
}
```

To put the credential to use, reference it from a grantable secret's `token_broker` source, then grant that secret to a principal:

```json
{
  "data": {
    "foreign_id": "gmail-auth",
    "inject_config": { "header": "Authorization", "formatter": "Bearer {{ .Value }}" },
    "source": { "source_type": "token_broker", "config": { "credential_id": "bcr_..." } },
    "rules": [ { "host": "gmail.googleapis.com" } ]
  }
}
```

### Re-authenticating a dead credential

When a refresh fails unrecoverably (for example the IdP returns `invalid_grant` because the refresh token was revoked), the credential's `status` becomes `dead` and it stops minting tokens. Supply a fresh `refresh_token` for a `refresh_token` credential, fresh `username` / `password` fields for a `password` credential, or fresh `username` / `api_key` fields for a `preqin` credential, via `PUT` / `PATCH` to clear the dead state and reschedule it:

```json
{ "data": { "refresh_token": "1//0gNEW..." } }
```

### Other operations

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/broker_credentials` | List. `labels[k]=v` and pagination are optional. |
| `GET`  | `/api/v1/broker_credentials/:id` | Fetch one. `404` if missing. |
| `GET`  | `/api/v1/broker_credentials/lookup/:foreign_id` | Fetch by foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/broker_credentials/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`. Fresh initial values reseed and clear dead state. Omitted fields are preserved; write-only fields are only changed when supplied. |
| `DELETE` | `/api/v1/broker_credentials/:id` | Delete. Returns `204`; `404` if missing. Returns `409` if any `token_broker` secret source still references the credential (remove those references first). |

## OAuth apps

An OAuth app registers an OAuth client (provider, client id, and client secret) and the scopes its [consent flow](#oauth-consent-flow) requests. The app's whole identity is a globally-unique `slug`: a team member who knows the integration (for example `google`) opens `/oauth/<slug>/start`, consents, and each completed consent mints (or updates) a [broker credential](#broker-credentials) linked back to the app. The minted credential is refreshed by the normal broker loop and delegates its `client_id` / `client_secret` to the app.

Only managing the app's configuration requires API key auth. The consent flow endpoints themselves are unauthenticated and live on iron-control's own domain (see [OAuth consent flow](#oauth-consent-flow)).

The `provider` field is validated against the [supported providers](#supported-providers), so an unknown provider returns `422`.

### Attributes

| Field                  | In requests | Notes |
| ---------------------- | ----------- | ----- |
| `slug`                 | required    | The app's identity: globally-unique, URL-safe, and the name in the well-known consent links (`/oauth/<slug>/start`). Must not start with the opaque-id prefix. |
| `description`          | optional    | |
| `labels`               | optional    | |
| `provider`             | required    | The provider strategy. See [supported providers](#supported-providers). |
| `provider_config`      | conditional | Required for `"custom"` and must be empty (`{}`) otherwise. An object with `authorization_endpoint`, `token_endpoint`, `resource`, and optionally `userinfo_endpoint`, each an `https` URL. |
| `client_id`            | required    | OAuth client id. Not secret; returned in responses. |
| `client_secret`        | required on create | OAuth client secret. Write-only and encrypted at rest; on update it is only changed when supplied. Never returned. |
| `allowed_scopes`       | required    | Non-empty array of scope strings the start endpoint requests. A flow's optional `scopes` param must be a subset; omitting it requests all of these. |
| `enabled`              | optional    | Defaults to `true`. A disabled app rejects new consent flows; existing credentials keep refreshing. |
| `shared`               | optional    | Defaults to `false`. One admin-connected account with role-granted access. Create-only; changes return `422`. Incompatible with `always_available`. |
| `always_available`     | optional    | Defaults to `false`. Use the connecting user's credential automatically whenever they ask the bot, not only in their DMs. |

Responses also include these read-only fields:

| Field               | Notes |
| ------------------- | ----- |
| `client_secret_set` | `true` when a client secret is stored. The secret itself is never returned. |
| `redirect_uri`      | The callback URI to register with the provider: `<public URL>/oauth/<slug>/callback`. The public URL is `CENTAUR_CONSOLE_PUBLIC_URL` when set, otherwise the request's base URL. |
| `start_url`         | The well-known consent link: `<public URL>/oauth/<slug>/start`. |
| `credentials_count` | How many broker credentials the app has minted. |

The `client_secret` is required and write-only: it is accepted on writes but never returned in any response.

### Create

`POST /api/v1/oauth_apps`

```json
{
  "data": {
    "slug": "google",
    "description": "Gmail",
    "provider": "google",
    "client_id": "1234.apps.googleusercontent.com",
    "client_secret": "GOCSPX-...",
    "allowed_scopes": ["https://www.googleapis.com/auth/gmail.readonly"]
  }
}
```

Returns `201`. The `client_secret` is never echoed back:

```json
{
  "data": {
    "id": "oap_...",
    "slug": "google",
    "description": "Gmail",
    "labels": {},
    "provider": "google",
    "client_id": "1234.apps.googleusercontent.com",
    "allowed_scopes": ["https://www.googleapis.com/auth/gmail.readonly"],
    "enabled": true,
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

### Other operations

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/oauth_apps` | List all apps. `labels[k]=v` and pagination optional. |
| `GET`  | `/api/v1/oauth_apps/:id` | Fetch one by OID. `404` if missing. |
| `GET`  | `/api/v1/oauth_apps/lookup/:slug` | Fetch by slug. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/oauth_apps/:id` | [Upsert](#upsert-put--patch) by OID or slug. Omitted fields are preserved; `client_secret` is only changed when supplied. |
| `DELETE` | `/api/v1/oauth_apps/:id` | Delete. Returns `204`; `404` if missing. Returns `409` while the app still has minted credentials (delete or unlink them first). |
| `GET`  | `/api/v1/oauth_apps/:id/credentials` | The broker credentials the app minted, oldest first. `labels[k]=v` and pagination optional. Each row has `id`, `foreign_id`, `name`, `status`, `provider_email`, `external_user_key`, and `created_at`; token material is never included. `404` if the app is missing. |
| `GET`  | `/api/v1/oauth_apps/:id/provider_details` | Provider-specific details for a shared app's connected account (its oldest credential). Returns `data: { provider, credential, details }`, where `credential` is `null` for personal apps or when nothing is connected, else `{ id, provider_subject, static_secret_id }` (`static_secret_id` is the wrapping static secret, or `null`). `details` is fetched live from the provider with 2-second timeouts, only while the credential is live with an unexpired access token (it is never refreshed). Today only `mercator` returns details (`{ claimed, ... }`). Any provider failure yields `{}`. |
| `POST` | `/api/v1/oauth_apps/presets/:provider` | Set up a provider preset. If the preset's app (keyed by its slug) does not exist yet, iron-control registers an OAuth client with the provider (RFC 7591 dynamic registration) and creates the app. Returns `200` with `data: { id, slug, start_url }`; send the admin to `start_url` to connect the account. `404` for providers without a preset, `409` when an existing app with the preset's slug has a different provider, shared mode, or is disabled, and `502` when registration fails (no app is created). |

### OAuth providers

`GET /api/v1/oauth_providers` (admin) lists the [supported providers](#supported-providers) in registry order. The first one is the console form's default.

```json
{
  "data": [
    { "key": "google", "display_name": "Google", "presettable": false, "config_keys": [], "optional_config_keys": [] },
    { "key": "custom", "display_name": "Custom", "presettable": false,
      "config_keys": ["authorization_endpoint", "token_endpoint", "userinfo_endpoint", "resource"],
      "optional_config_keys": ["userinfo_endpoint"] }
  ]
}
```

`config_keys` lists the `provider_config` keys the provider takes. `presettable` is `true` when the provider can be set up with `POST /api/v1/oauth_apps/presets/:provider`.

### Integrations

`GET /api/v1/integrations` serves the console Integrations page. Unlike the rest of this section, any active console user may call it, with a session cookie or an API key. It returns the OAuth apps the caller can connect and their connection state:

```json
{
  "data": {
    "apps": [
      {
        "id": "oap_...", "slug": "google", "provider": "google", "description": "Gmail",
        "shared": false, "always_available": false, "usage": "direct_messages",
        "start_url": "https://<iron-control>/oauth/google/start",
        "connection": { "status": "connected" }
      }
    ],
    "presets": [{ "provider": "mercator", "display_name": "Mercator", "description": "Shared with your agents." }],
    "ssh_key": { "configured": false }
  }
}
```

- `apps` lists enabled apps ordered by slug. Shared apps appear only for acting admins (a descoped admin sees what an operator sees). The client id, client secret, scopes, and labels are never included.
- `usage` is `shared`, `always_available`, or `direct_messages` (the app is used only in the user's DMs with the bot).
- `connection` is `null` when the caller has no credential for the app. Otherwise `status` is `connected` or `needs_reconnect` (the credential is dead or is missing a scope the app requires). The caller's credentials are the ones they minted while signed in, plus any whose provider email matches their console email. For a shared app, any credential counts. When several match, the most recently updated one wins.
- `presets` lists providers with a preset that have no app yet. It is empty unless the caller is an acting admin.
- `ssh_key.configured` reports whether the caller saved a personal SSH key.

### Sandbox Start URLs

`GET /api/v1/sandbox/oauth_apps`

Returns enabled non-shared OAuth apps and the console URLs a sandbox user can open to start consent. Authenticate with the same sandbox entitlement JWT as `GET /api/v1/sandbox/permissions`. The token signature, issuer, audience, and expiry are verified. Proxy and principal claims are not checked because these URLs are not sensitive.

```json
{
  "data": [
    {
      "id": "oap_...",
      "slug": "google",
      "description": "Gmail",
      "labels": {},
      "provider": "google",
      "allowed_scopes": ["https://www.googleapis.com/auth/gmail.readonly"],
      "start_url": "https://<iron-control>/oauth/google/start"
    }
  ]
}
```

### Sandbox OAuth Credential Metadata

`GET /api/v1/sandbox/permissions`

The sandbox permissions response includes an `oauth_credentials` array with non-secret metadata for OAuth-flow credentials currently granted to the sandbox principal. Use it to confirm that a user completed consent for the expected app and personal email. It also includes a sorted `connected_tools` array derived from the `centaur-tool` labels on credentials granted directly or through roles.

```json
{
  "data": {
    "connected_tools": ["gsuite"],
    "oauth_credentials": [
      {
        "id": "bcr_...",
        "oauth_app_id": "oap_...",
        "slug": "google",
        "provider": "google",
        "provider_email": "person@example.com",
        "provider_subject": "google-subject",
        "status": "live",
        "scopes": ["https://www.googleapis.com/auth/gmail.readonly"]
      }
    ]
  }
}
```

### Sandbox Admin Permission Management

An active admin Console user's Slack DM sandbox may create Slack channel, Slack user, and workflow principals, create and edit static, Postgres DSN, and GCP ID token secrets whose sources reference an external store, grant secrets to roles, inspect a principal's effective grants, manage its roles, direct secret grants, Slack channel permissions, and sandbox capabilities, and edit the Slack Connect allowlist with its sandbox entitlement JWT. The sandbox principal must be a `slack_dm` principal linked to that admin whose Slack team is the bot's workspace, as recorded by the Slack channel catalog. Other sandboxes, including channel, Slack Connect, and Console-user sessions, receive `403`. Request and response shapes match the corresponding admin endpoints.

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET` | `/api/v1/sandbox/admin/principals` | List principals. `slack_channel_id=<id>` returns only principals bound to that channel. |
| `GET` | `/api/v1/sandbox/admin/principals/:id` | Fetch one by OID. |
| `POST` | `/api/v1/sandbox/admin/principals` | Create a Slack channel (`data: { kind: "slack_channel", slack_channel_id }`), Slack user (`data: { kind: "slack_dm", slack_user_id }`), or workflow (`data: { kind: "workflow", workflow_name }`) principal. The foreign ID, kind, and Slack fields match what api-rs derives: `slack-channel-<channel>`, `slack-user-<team>-<user>` in the admin's workspace, or `workflow-<slugged workflow_name>` for `WORKFLOW_PRINCIPAL = True`. A new channel principal gets full Slack access to its own channel. Other kinds and DM channel IDs return `400`; invalid IDs and existing principals return `422`. |
| `GET` | `/api/v1/sandbox/admin/principals/lookup/:foreign_id` | Fetch one by foreign ID. |
| `GET`/`POST` | `/api/v1/sandbox/admin/static_secrets[/:id]`, `/api/v1/sandbox/admin/pg_dsn_secrets[/:id]`, `/api/v1/sandbox/admin/gcp_id_token_secrets[/:id]` | Fetch a secret by OID or create one, as in the static, Postgres DSN, and GCP ID token secret APIs. Inline `control_plane` and OAuth `token_broker` sources return `403`. |
| `PATCH`/`PUT` | `/api/v1/sandbox/admin/static_secrets/:id`, `/api/v1/sandbox/admin/pg_dsn_secrets/:id`, `/api/v1/sandbox/admin/gcp_id_token_secrets/:id` | Update an existing secret by OID. Omitted fields, including the source, keep their current values. Same source restrictions; secrets with a `token_broker` source return `403`. |
| `GET`/`POST` | `/api/v1/sandbox/admin/principals/:principal_id/roles` | List or assign roles (`data: { role_id }`). |
| `DELETE` | `/api/v1/sandbox/admin/principals/:principal_id/roles/:id` | Unassign a role. |
| `GET` | `/api/v1/sandbox/admin/roles`, `/roles/:id`, `/roles/lookup/:foreign_id` | List or fetch roles. |
| `GET` | `/api/v1/sandbox/admin/roles/:role_id/grants` | List a role's grants. |
| `GET` | `/api/v1/sandbox/admin/principals/:principal_id/grants` | List a principal's direct grants. |
| `GET` | `/api/v1/sandbox/admin/principals/:id/effective_grants` | List secrets the principal holds directly or through roles, paginated with `page` and `limit`. Each row has `id`, `grant_key`, `foreign_id`, `name`, `direct`, and `roles` (`id`, `foreign_id`, `name`) that grant it. |
| `POST` | `/api/v1/sandbox/admin/grants` | Grant a secret to a principal (`data: { principal_id, <grant_key>: <secret oid> }`) or a role (`data: { role_id, <grant_key>: <secret oid> }`). A role grant reaches every holder of the role. |
| `DELETE` | `/api/v1/sandbox/admin/grants/:id` | Revoke a principal or role grant. |
| `GET` | `/api/v1/sandbox/admin/secrets` | List grantable secrets of every kind with `id`, `grant_key`, `foreign_id`, `name`, `description`, and `labels`. `q` filters by name or foreign ID. |
| `POST` | `/api/v1/sandbox/admin/principals/:id/slack_channel_permissions` | Upsert one Slack channel permission (`data: { channel_id, upload_enabled, download_enabled, history_enabled }`). New permissions default omitted flags to `true`. |
| `DELETE` | `/api/v1/sandbox/admin/principals/:id/slack_channel_permissions/:channel_id` | Remove the principal's own permission for a channel. |
| `PATCH` | `/api/v1/sandbox/admin/principals/:id/sandbox_access` | Update only the supplied `sandbox_repo_cache` and `sandbox_*_enabled` fields. Flags must be JSON booleans; other values return `400`. |
| `GET` | `/api/v1/sandbox/admin/slack_external_teams` | List allowed Slack Connect workspace (`T…`) and Enterprise Grid org (`E…`) ids as `data: [id, …]`. The list is stored by api-rs; if it is unreachable the request returns `503`. |
| `POST` | `/api/v1/sandbox/admin/slack_external_teams` | Allow one id (`data: { team_id }`) and return the updated list. Adding an allowed id changes nothing. Invalid ids return `422`. |
| `DELETE` | `/api/v1/sandbox/admin/slack_external_teams/:id` | Remove one id and return the updated list. Returns `404` if the id is not allowed. |

## OAuth consent flow

The consent flow turns a team member's OAuth consent into a managed broker credential. It runs on iron-control's own domain and is deliberately unauthenticated: the member reaches it with a single well-known link keyed by the app's `slug`. There is no external app to integrate with, so the start endpoint takes no `user` or `return_to`: after consent the member lands on an iron-control result page, and the credential's `external_user_key` is generated automatically. Safety comes from the consent itself (a credential is only created after a successful code exchange) and upsert-on-reconsent (re-consenting for the same provider account updates the existing credential instead of creating a new one).

One redirect URI is registered with the IdP per app, keyed by its slug:

```
https://<iron-control>/oauth/<slug>/callback
```

By default the redirect URI and the flow's own origin are derived from the request. Set `CENTAUR_CONSOLE_PUBLIC_URL` to override the origin for deployments behind a proxy whose `Host` header does not match the public origin.

### Start

```
GET https://<iron-control>/oauth/<slug>/start
```

`<slug>` names the app (for example `google`). The provider is derived from the app.

| Param    | Notes |
| -------- | ----- |
| `scopes` | Optional, space- or comma-separated. Must be a subset of the app's `allowed_scopes`; defaults to all of them. |

On success the endpoint redirects the browser to the provider's consent screen. An unknown slug returns `404`; a disabled app or a scope outside the allowlist renders a `4xx` result page.

### Callback and result page

After consent the provider redirects back to `/oauth/<slug>/callback`, which exchanges the code, mints or updates the credential, and renders an iron-control result page:

| Outcome  | Page | Status |
| -------- | ---- | ------ |
| Success  | Confirms the integration is connected and shows the credential OID. | `200` |
| Denied   | The user declined (or another IdP-side error). | `422` |
| Error    | The code exchange or identity check failed (e.g. `invalid_grant`). | `422` |

A tampered, expired, or missing flow state or cookie renders an error page with `400`.

### Supported providers

| Provider  | `provider` value |
| --------- | ---------------- |
| Attio     | `attio`          |
| Custom    | `custom`         |
| GitHub    | `github`         |
| Google    | `google`         |
| Granola   | `granola`        |
| Linear    | `linear`         |
| Mercator  | `mercator`       |
| Microsoft | `microsoft`      |
| Slack     | `slack`          |
| Zoom      | `zoom`           |

The `custom` provider connects an authorization server that needs no provider-specific code, such as a remote MCP server whose client was registered in advance. It runs the authorization-code flow with PKCE (`S256`), authenticates to the token endpoint with `client_secret_post`, and sends `resource` as the RFC 8707 resource indicator on the authorization, token, and refresh requests. The account identity is the `sub` (plus `email` and `name`, when present) returned by `userinfo_endpoint`. Without a `userinfo_endpoint`, the access token must be a JWT, and the identity comes from its claims. The minted token is injected only for requests to the host and path of `resource`, and refreshes keep the granted scopes.

Slack OAuth apps may use token rotation or long-lived tokens. When token rotation is enabled, the callback stores the returned refresh token and the broker refresh loop keeps the access token fresh. When Slack returns a long-lived token without a refresh token or expiry, the credential is stored without scheduling broker refresh.
Slack OAuth apps should use normal Slack API scopes such as `channels:history`, not Sign in with Slack scopes such as `openid`, `email`, or `profile`.

## Principals

A principal is an identity (an application, service, or proxy owner) that can be granted secrets.

When a principal is created with no preassigned roles, the console assigns the
system default roles. Defaults apply
only during initial creation. Updating an existing roleless principal does not
restore roles that an operator removed. The default configuration assigns the
system-managed `infra` role.

### Attributes

| Field        | In requests | Notes |
| ------------ | ----------- | ----- |
| `foreign_id` | optional    | Globally unique. Immutable. |
| `name`       | optional    | |
| `kind`       | optional    | Defaults to `unknown`. See the known values below. |
| `slack_user_id` | optional | First-class Slack user identity. |
| `slack_channel_id` | optional | First-class Slack conversation identity. |
| `slack_team_id` | optional | First-class Slack team or enterprise scope. |
| `slack_email` | optional | First-class Slack email identity. |
| `console_user_id` | optional | Database ID of the associated Console user. A `slack_dm` principal is linked automatically when an explicitly supplied `slack_email` matches a Console user's email. |
| `labels`     | optional    | Extensible metadata. Identity is read from the first-class fields, not from labels with matching names. |
| `slack_channel_permissions` | optional | Direct permissions owned by the principal. Full replacement when present on create or update. |
| `effective_slack_channel_permissions` | response only | Direct permissions merged with permissions inherited from assigned roles. |

Known kinds are `unknown`, `user`, `console_user`, `workflow`,
`slack_channel`, `slack_dm`, `discord_channel`, `linear_issue`, `teams_user`,
and `teams_conversation`. Use one of these values for the `kind` field.

### Operations

`POST /api/v1/principals`

```json
{ "data": { "foreign_id": "api-service", "name": "API Service", "labels": { "tier": "backend" } } }
```

Returns `201`:

```json
{
  "data": {
    "id": "prn_...",
    "foreign_id": "api-service",
    "name": "API Service",
    "labels": { "tier": "backend" },
    "slack_channel_permissions": [],
    "effective_slack_channel_permissions": [],
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/principals` | List. Accepts exact-match label filters. |
| `GET`  | `/api/v1/principals/:id` | Fetch one by OID. To fetch by `foreign_id`, use the lookup route below. |
| `GET`  | `/api/v1/principals/lookup/:foreign_id` | Fetch by foreign id. `404` if missing. |
| `GET`  | `/api/v1/principals/:id/effective_config` | [Effective config](#effective-config) the principal resolves to. `:id` is an OID. |
| `GET`  | `/api/v1/principals/lookup/:foreign_id/effective_config` | [Effective config](#effective-config) by foreign id. `404` if missing. |
| `GET`  | `/api/v1/principals/:principal_id/grants` | [List the grants](#list-by-grantee) granted directly to the principal. |
| `POST` | `/api/v1/principals/:id/slack_channel_permissions` | Idempotently create or update one direct Slack channel permission. Omitted flags default to enabled on create and remain unchanged on update. |
| `PUT`/`PATCH` | `/api/v1/principals/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`. `name`, first-class identity fields, `labels`, and direct `slack_channel_permissions` are mutable on an existing record. `foreign_id` applies only when creating. |
| `DELETE` | `/api/v1/principals/:id` | Delete by OID. Removes direct grants, role assignments, Slack channel permissions, scheduled tasks, MCP OAuth codes and refresh tokens, and sync snapshots; assigned proxies are unassigned. Returns `204`. |
| `GET`  | `/api/v1/principals/:id/effective_grants` | Paginated secrets the principal resolves to, directly or through roles. See below. |
| `PUT`  | `/api/v1/principals/:id/slack_channel_permissions` | Replace the direct Slack channel permissions with `data.slack_channel_permissions`. DM rows (`D…`) are API-managed: stored DM rows are kept and submitted DM rows ignored. An unchanged submission writes nothing. Returns the stored rows. |
| `DELETE` | `/api/v1/principals/:id/slack_channel_permissions/:channel_id` | Delete one direct Slack channel permission. `204`; `404` if absent. |

Console-only list and fetch options (additive):

- `GET /api/v1/principals?q=<text>` matches `name` or `foreign_id`
  case-insensitively.
- `GET /api/v1/principals?sort=name` orders by `name` (unnamed last), then
  creation time. Any other `sort` value returns `400`.
- `view=console` on the list and fetch endpoints adds `kind`,
  `slack_user_id`, `roles` (`[{ id, foreign_id, name }]`), and
  `inherited_slack_channel_permissions` (permissions merged from assigned
  roles). The default payload omits them.

`GET /api/v1/principals/:id/effective_grants` returns:

```json
{
  "data": [
    {
      "id": "ssr_...",
      "kind": "static",
      "grant_key": "static_secret_id",
      "foreign_id": "github-token",
      "name": "GitHub token",
      "direct": true,
      "roles": [{ "id": "role_...", "foreign_id": "infra", "name": "Infra" }]
    }
  ],
  "meta": { "page": 1, "limit": 50, "total": 1, "total_pages": 1 }
}
```

Identity values must be sent through the first-class fields. Labels with the
same names are ordinary metadata and do not change or mirror identity fields.
A null or blank `kind` is rejected, as are unknown kind values and new or
changed malformed nonblank Slack identities. Unchanged migrated values remain
round-trip safe.

See [Role assignments](#role-assignments) for attaching roles to a principal.

### Effective config

`GET /api/v1/principals/:id/effective_config`
`GET /api/v1/principals/lookup/:foreign_id/effective_config`

The config a principal resolves to, in the same shape `iron-proxy` receives on [proxy sync](#proxy-sync), for operator inspection. The principal is addressed by OID (`:id`) or by `foreign_id` via the lookup route.

Unlike proxy sync, this endpoint never reveals live secrets and does no config-hash negotiation:

- Inline `control_plane` source values are redacted to `"[redacted]"`. Every other source type carries only a reference (an env var name, a `secret_id`, ...), so it passes through unchanged.
- There is no `config_hash`, `status`, or `principal_id` field, and no hash request param.
- The response carries a content-derived `ETag` for change detection and `Cache-Control: no-store`, so it is never served from a cache.

Returns `200`:

```json
{
  "data": {
    "id": "prn_...",
    "secrets": [
      {
        "source": { "type": "env", "var": "GITHUB_TOKEN" },
        "inject": { "header": "Authorization", "formatter": "Bearer {{ .Value }}" },
        "rules": [ { "host": "api.github.com", "methods": ["GET", "POST"], "paths": ["/repos/*"] } ]
      },
      {
        "source": { "type": "control_plane", "value": "[redacted]" },
        "replace": { "proxy_value": "__DB_PASSWORD__" },
        "rules": [ { "host": "db.internal", "methods": ["*"] } ]
      }
    ],
    "transforms": [],
    "postgres": []
  }
}
```

The `secrets`, `transforms`, and `postgres` arrays are assembled exactly as in [proxy sync](#proxy-sync), covering the principal's effective grants (direct plus any held via a [role](#roles)). See that section for the per-field details.

## Roles

A role is a reusable bundle of [grants](#grants) and Slack channel permissions. Principals are assigned roles, and a principal's effective access is the union of its own direct grants and Slack permissions plus those of every role it holds.

### Attributes

| Field        | In requests | Notes |
| ------------ | ----------- | ----- |
| `foreign_id` | optional    | Globally unique. Immutable. Handy for idempotent provisioning. |
| `name`       | optional    | |
| `labels`     | optional    | |
| `slack_channel_permissions` | optional | Full replacement when present on create or update. Each row accepts `channel_id` and the `upload_enabled`, `download_enabled`, and `history_enabled` flags. |

### Operations

`POST /api/v1/roles`

```json
{ "data": { "foreign_id": "infra", "name": "Infra", "labels": { "kind": "shared" } } }
```

Returns `201`:

```json
{
  "data": {
    "id": "role_...",
    "foreign_id": "infra",
    "name": "Infra",
    "labels": { "kind": "shared" },
    "slack_channel_permissions": [],
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

| Method   | Path | Notes |
| -------- | ---- | ----- |
| `GET`    | `/api/v1/roles` | List. `labels[k]=v` and pagination are optional. `q` matches `name` or `foreign_id` (case-insensitive substring). `exclude_principal=<principal oid>` omits the roles already assigned to that principal, across all pages (the Console assign-role picker); an unknown principal is a `404`. |
| `GET`    | `/api/v1/roles/:id` | Fetch one. |
| `GET`    | `/api/v1/roles/lookup/:foreign_id` | Fetch by foreign id. `404` if missing. |
| `GET`    | `/api/v1/roles/:role_id/grants` | [List the grants](#list-by-grantee) attached to the role. |
| `POST`   | `/api/v1/roles/:id/slack_channel_permissions` | Idempotently create or update one role-owned Slack channel permission without replacing other rows. Omitted flags default to enabled on create and remain unchanged on update. |
| `PUT`/`PATCH` | `/api/v1/roles/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`. `name`, `labels`, and `slack_channel_permissions` are mutable on an existing record; `foreign_id` applies only when creating. |
| `PUT`    | `/api/v1/roles/:id/slack_channel_permissions` | Replace the role's Slack channel permissions with `data.slack_channel_permissions`. Rows are keyed by `channel_id`. An unchanged submission writes nothing; a change bumps the sync config of every principal holding the role. Returns the stored rows. `422` if a row has no permission enabled. |
| `DELETE` | `/api/v1/roles/:id/slack_channel_permissions/:channel_id` | Delete one role Slack channel permission. `204`; `404` if absent. |
| `DELETE` | `/api/v1/roles/:id` | Delete. Returns `204`. Cascades: the role's grants and its assignments are removed. |

### Role assignments

Assign and unassign roles on a principal. The assignment endpoints are nested under the principal; the role is identified by its OID.

`POST /api/v1/principals/:principal_id/roles`

```json
{ "data": { "role_id": "role_..." } }
```

Returns `201` with the assigned role's representation. Assigning an already assigned role returns `422`. An unknown principal or role returns `404`.

| Method   | Path | Notes |
| -------- | ---- | ----- |
| `GET`    | `/api/v1/principals/:principal_id/roles` | List the roles assigned to the principal. |
| `POST`   | `/api/v1/principals/:principal_id/roles` | Assign a role (`data: { role_id }`). |
| `DELETE` | `/api/v1/principals/:principal_id/roles/:id` | Unassign the role with OID `:id`. Returns `204`; `404` if not assigned. |

## Grants

A grant attaches exactly one secret to a **grantee** — either a principal or a [role](#roles). A principal receives a secret if it is granted directly or through any role the principal holds; its proxies then receive that secret through [proxy sync](#proxy-sync).

### Create

`POST /api/v1/grants` — supply exactly one grantee (`principal_id` **or** `role_id`) plus exactly one of `static_secret_id`, `gcp_auth_secret_id`, `aws_auth_secret_id`, `oauth_token_secret_id`, `pg_dsn_secret_id`, `hmac_secret_id`, or `ssh_key_secret_id`:

```json
{ "data": { "principal_id": "prn_...", "static_secret_id": "ssr_..." } }
```

Or grant to a role:

```json
{ "data": { "role_id": "role_...", "static_secret_id": "ssr_..." } }
```

Returns `201`. The response includes the one grantee key and the one secret-type key that were set:

```json
{
  "data": {
    "id": "grant_...",
    "principal_id": "prn_...",
    "static_secret_id": "ssr_...",
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

Referencing a missing grantee or secret returns `404`. Supplying no grantee returns `422` with `"must reference one of principal_id, role_id"`; supplying no secret returns `422` with `"must reference one of static_secret_id, gcp_auth_secret_id, aws_auth_secret_id, oauth_token_secret_id, pg_dsn_secret_id, hmac_secret_id, ssh_key_secret_id"`.

### List by grantee

List the grants attached to a single grantee. The endpoints are nested under the grantee, which is identified by its OID. The grantee is resolved first, so an unknown principal or role returns `404` rather than an empty list; a grantee with no grants returns `200` with an empty `data` array.

`GET /api/v1/principals/:principal_id/grants`

Returns `200`. Results use the standard [paginated](#conventions) envelope, and each entry has the same shape as `GET /api/v1/grants/:id`:

```json
{
  "data": [
    {
      "id": "grant_...",
      "principal_id": "prn_...",
      "static_secret_id": "ssr_...",
      "grantable": { "kind": "static", "kind_label": "Static", "id": "ssr_...", "foreign_id": "github-token", "name": "GitHub token" },
      "created_at": "2026-06-01T10:00:00Z",
      "updated_at": "2026-06-01T10:00:00Z"
    }
  ],
  "meta": { "page": 1, "limit": 50, "total": 1, "total_pages": 1 }
}
```

Grant payloads (here and from `GET`/`POST /api/v1/grants`) include a
`grantable` summary of the granted secret: its kind slug and label, OID,
`foreign_id`, and `name`. It never includes sources or values.

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/principals/:principal_id/grants` | List the grants granted directly to the principal. Paginated; `404` if the principal is unknown. |
| `GET`  | `/api/v1/roles/:role_id/grants` | List the grants attached to the role. Paginated; `404` if the role is unknown. |

The principal endpoint lists only the principal's **direct** grants, not those it resolves through roles. For everything a principal resolves to, see [effective config](#effective-config).

### Other operations

| Method   | Path | Notes |
| -------- | ---- | ----- |
| `GET`    | `/api/v1/grants/:id` | Fetch one. Response carries `principal_id` or `role_id` depending on the grantee. |
| `DELETE` | `/api/v1/grants/:id` | Revoke. Returns `204`. |

## Secrets (all kinds)

`GET /api/v1/secrets` lists grantable secrets of every kind in one paginated
list, ordered by name (case-insensitive) then creation time. Only identifying
metadata, source backend labels, and the managing integration are returned,
never source references or values.

| Param | Notes |
|---|---|
| `q` | Matches `name` or `foreign_id` case-insensitively. |
| `kind` | One of `static`, `gcp_auth`, `gcp_id_token`, `aws_auth`, `oauth_token`, `pg_dsn`, `hmac`, `ssh_key`; anything else returns `400`. |
| `exclude_grantee` | `principal:<oid>` or `role:<oid>`; omits secrets granted directly to that grantee (a principal's role-inherited secrets stay listed). `404` for an unknown grantee; other values return `400`. |
| `page`, `limit` | Standard pagination. |

```json
{
  "data": [
    {
      "kind": "static", "kind_label": "Static", "id": "ssr_...", "foreign_id": "github-token",
      "name": "GitHub token", "grant_key": "static_secret_id", "description": null,
      "labels": {}, "source_types": ["Env"], "managed_by": null,
      "created_at": "...", "updated_at": "..."
    }
  ],
  "meta": { "page": 1, "limit": 50, "total": 1, "total_pages": 1 }
}
```

`source_types` lists the distinct source backends as display labels (`Env`,
`AWS-SM`, `AWS-SSM`, `1Password`, `1Password-Connect`, `Inline`, `Token-Broker`,
`Workload-Identity`); it never includes references. `managed_by` is set when a
static secret wraps a broker credential minted by an OAuth flow, and is `null`
otherwise:

```json
{ "broker_credential_id": "bcr_...", "oauth_app_id": "oap_...", "oauth_app_slug": "google" }
```

Use `grant_key` as the secret field when creating a [grant](#grants).

### Secret detail (console view)

`GET /api/v1/secrets/:kind/:id` returns one secret of any kind (`:kind` is a
slug from the table above, `:id` its opaque id) as the console detail page shows
it. It carries every field of the list item plus:

| Field | Notes |
|---|---|
| `editable` | `true` for the kinds the console creates and edits (`static`, `gcp_auth`, `gcp_id_token`, `pg_dsn`). |
| `injection` | Where the resolved value goes, e.g. `"Authorization"`, `"?api_key"`, `"x-serverless-authorization: Bearer"`; `null` for PG DSN secrets. |
| `source_segments` | `[{ role, type, type_label, ref }]`: where the value is resolved from. `ref` is the reference within the backend (env var, secret id, 1Password ref, ...). An inline (`control_plane`) value is shown as the literal `"inline"`, never the value. `role` names the credential field for OAuth token, HMAC, and AWS secrets. |
| `configuration` | `[{ label, value }]` display rows ("Injects into", "Credential profile", "Mode", "Scopes", "Session settings", ...). Multi-value fields are joined with newlines. |
| `signed_headers` | HMAC secrets: `[{ name, value }]` header templates; `null` for other kinds. |
| `rules` | The [request rules](#request-rules) for kinds that have them; `null` otherwise. |

An unknown kind or id is a `404`.

### Role grants of a secret

The roles a secret is granted to, addressed from the secret. These are ordinary
[grants](#grants); the routes only scope them to one secret, so a revoke cannot
remove a grant of a different secret or a direct principal grant.

| Method | Path | Notes |
|---|---|---|
| `GET` | `/api/v1/secrets/:kind/:secret_id/role_grants` | `{ data: [grant], assignable_roles: [role] }`. Grants are in creation order; `assignable_roles` are the roles not yet granted this secret. |
| `POST` | `/api/v1/secrets/:kind/:secret_id/role_grants` | Body `{ "data": { "role_id": "role_..." } }`. Grants at the default role priority. Returns `201`, or `200` with the existing grant when the role already has it. An unknown role is a `404`. |
| `DELETE` | `/api/v1/secrets/:kind/:secret_id/role_grants/:id` | Revoke. Returns `204`; `404` unless `:id` is a role grant of this secret. |

A grant object:

```json
{
  "id": "grant_...", "role_id": "role_...",
  "role": { "id": "role_...", "foreign_id": "infra", "name": "Infra" },
  "grantable": { "kind": "static", "kind_label": "Static", "id": "ssr_...", "foreign_id": "github-token", "name": "GitHub token" },
  "priority": 0, "created_at": "...", "updated_at": "..."
}
```

A role is `{ id, foreign_id, name }`.

## Slack channels

`GET /api/v1/slack_channels` searches the Slack channel catalog for channel
pickers (admin-only, `Cache-Control: no-store`). At most 20 active channels are
returned.

| Param | Notes |
|---|---|
| `q` | Name or channel ID fragment. |
| `exclude_owner` | `principal:<oid>` or `role:<oid>`; omits channels that owner already has a permission row for. Other values return `400`. |
| `ids[]` | Resolve these channel IDs (including archived channels) instead of searching. |

```json
{
  "data": [
    { "channel_id": "C0123456789", "name": "general", "private": false,
      "label": "#general", "description": "C0123456789 · Public" }
  ],
  "meta": { "catalog_error": null }
}
```

`meta.catalog_error` explains a missing bot token or a catalog that is still
loading.

## API keys

API keys belong to the authenticated user and authenticate API requests. They are scoped to the current user: listing and fetching only ever return your own keys.

### Create

`POST /api/v1/api_keys`

```json
{ "data": { "name": "CI Runner" } }
```

Returns `201`. The plaintext `token` is included **only** in this create response: save it immediately.

```json
{
  "data": {
    "id": "ak_...",
    "name": "CI Runner",
    "token": "iak_0a1b2c3d...",
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

`name` is required; omitting it returns `422`.

### Other operations

| Method   | Path | Notes |
| -------- | ---- | ----- |
| `GET`    | `/api/v1/api_keys` | List your keys (paginated). Tokens are never returned. |
| `GET`    | `/api/v1/api_keys/:id` | Fetch one (no token). |
| `DELETE` | `/api/v1/api_keys/:id` | Revoke (soft delete). Returns `204`. Revoking the key used for the current request returns `422` with `"cannot revoke the API key used for this request"`. |

## Users

Console operator accounts, for the console Users page. Admin-only. Users are
addressed by `usr_…` oid.

| Method | Path | Notes |
|---|---|---|
| `GET` | `/api/v1/users` | Every user, unpaginated, admins first then by email. Optional `status` (`pending`, `active`, `disabled`; anything else returns `400`). |
| `POST` | `/api/v1/users/:id/approve` | Activate a pending user and record the approver. |
| `POST` | `/api/v1/users/:id/disable` | Revoke access. Also revokes the user's MCP OAuth refresh tokens; their console session is signed out on its next request. Your own account returns `422` (`"You can't disable your own account."`, `details.reason = "self"`). |
| `POST` | `/api/v1/users/:id/promote` | Make the user an admin. This also activates the account. |
| `POST` | `/api/v1/users/:id/demote` | Remove admin. Your own account returns `422` (`"You can't demote your own account."`, `details.reason = "self"`). |

Each action returns the updated user as `{ "data": user }`. The list
response looks like this:

```json
{
  "data": [
    {
      "id": "usr_…", "email": "ops@example.com", "name": "Ops", "status": "active",
      "admin": true, "current": true, "identity_providers": ["google"],
      "approved_at": null, "approved_by": null,
      "created_at": "…", "updated_at": "…"
    }
  ],
  "meta": { "total": 1 }
}
```

`current` is `true` for the caller's own account. `identity_providers` lists
the distinct linked SSO providers, sorted; it is empty for password-only
accounts. `approved_by` is `{ "id", "email" }` or `null`.

## API clients

Machine callers of the api-rs session API, such as eval runners. Each client
runs sessions as one of its attached principals. Admin-only. While
`CENTAUR_CONSOLE_API_CLIENTS_ENABLED` is off, every endpoint returns `404`
`{"error":{"message":"not found"}}`, before authentication, so the feature is
not advertised. Clients are addressed by `acl_…` oid (the client id), and
principal links by `aclp_…` oid.

```json
{
  "data": {
    "id": "acl_…", "name": "eval-runner", "scopes": ["sessions:read", "sessions:write"],
    "disabled": false, "disabled_at": null, "principals_count": 1,
    "principals": [
      { "id": "aclp_…", "default": true,
        "principal": { "id": "prn_…", "foreign_id": "evals", "name": "Evals", "kind": "user" } }
    ],
    "created_at": "…", "updated_at": "…"
  }
}
```

The client secret (`acs_…`) appears as `data.secret` **only** in the
responses to create and `rotate_secret`. Those responses are sent with
`Cache-Control: no-store`. The console stores only a digest, so no endpoint
can return the secret again.

| Method | Path | Notes |
|---|---|---|
| `GET` | `/api/v1/api_clients` | Every client, unpaginated, by creation order (`meta.total`). |
| `GET` | `/api/v1/api_clients/:id` | One client. |
| `POST` | `/api/v1/api_clients` | `{"data": {"name", "scopes": [...], "principal"}}`. `scopes` is a subset of `sessions:read` and `sessions:write`; blank entries are dropped, and at least one scope is required. The optional `principal` (an oid or a foreign ID) is attached as the default. If it matches nothing, the request returns `422` with `details.base = ["No principal matches \"x\"."]` and no client is created. Returns `201` with `secret`. |
| `PATCH` | `/api/v1/api_clients/:id` | Update `name` and/or `scopes`; omitted fields are kept. Never changes the secret. |
| `DELETE` | `/api/v1/api_clients/:id` | Delete the client and its principal links. Returns `204`. |
| `POST` | `/api/v1/api_clients/:id/rotate_secret` | Issue a new secret. The previous secret and its access tokens stop working immediately. Returns the client with `secret`. |
| `POST` | `/api/v1/api_clients/:id/disable` | Disable (idempotent). The client's access tokens stop working. |
| `POST` | `/api/v1/api_clients/:id/enable` | Re-enable. |
| `POST` | `/api/v1/api_clients/:id/principals` | `{"data": {"principal": "<oid or foreign ID>"}}`. Attaches the principal and returns `201` with the link. The first link becomes the default, and attaching an already attached principal returns the existing link. If the principal matches nothing, the request returns `422` (`"No principal matches that OID or foreign ID."`). |
| `DELETE` | `/api/v1/api_clients/:id/principals/:link_id` | Detach. Sessions already bound to that principal stop accepting turns. Returns `204`. |
| `POST` | `/api/v1/api_clients/:id/principals/:link_id/default` | Make this link the default principal. Returns the link. |

A `link_id` that belongs to another client returns `404`.

## System settings

Deployment-wide defaults for new control-plane records. Admin-only.

`GET /api/v1/system_settings`:

```json
{
  "data": {
    "default_sandbox_repo_cache": "all",
    "default_sandbox_observability_enabled": true,
    "default_sandbox_sessions_read_enabled": false,
    "default_sandbox_workflows_read_enabled": false,
    "default_sandbox_workflows_write_enabled": false,
    "default_role_ids": ["role_…"],
    "roles": [{ "id": "role_…", "foreign_id": "infra", "name": "Infra", "assign_by_default": true }],
    "updated_at": "…"
  }
}
```

`roles` lists every role (by name, then foreign ID) for the default-role
picker. New principals that have no preassigned roles receive the roles in
`default_role_ids`.

`PATCH /api/v1/system_settings` takes `{"data": {...}}` with any of the
`default_sandbox_*` fields (`default_sandbox_repo_cache` is one of `none`,
`public`, `all`) and `default_role_ids`, a list of role oids that replaces the
default set. `[]` clears the set. If `default_role_ids` is omitted, the
current defaults are kept. Invalid values and unknown role oids return `422`
and change nothing. The response has the same shape as `GET`.

### Slack Connect workspaces

The allowlist of external Slack workspaces whose messages and actions
Centaur accepts. api-rs stores it and Slackbot reads it; the console proxies
it.

| Method | Path | Notes |
|---|---|---|
| `GET` | `/api/v1/system_settings/slack_external_teams` | `{"data": {"slack_external_team_ids": ["T0…"]}}`. Returns `503` if the Centaur API is unreachable. |
| `PUT` | `/api/v1/system_settings/slack_external_teams` | `{"data": {"slack_external_team_ids": [...]}}` replaces the whole list. The value may also be one string split on whitespace and commas. Returns the stored list. A validation error from the Centaur API (HTTP 400) returns `422` with its message; any other failure returns `503`. |

## Scheduled Tasks

Tasks run one agent prompt and deliver the result to Slack. A task may have a five-field cron schedule in Pacific Time; omitting `cron_expression` or sending `null` or an empty string makes it manual-only. Task IDs use the `tsk_` prefix. The `delivery_channel` can be a Slack channel ID available to the task author, or the special value `dm`, which resolves to the linked user's Slack direct message when the task is created or updated.

These endpoints use the sandbox entitlement JWT injected by `iron-proxy`. For most sandboxes, every operation requires an active Console user linked to the sandbox principal and is scoped to tasks owned by that user. A task owned by another user returns `404`.

When the sandbox principal is a Slack channel, the endpoints manage that channel's tasks instead. Any Slack user from a workspace the bot is installed in who requests the current turn may list, create, update, delete, or run them; no Console account is required. A turn without such a requester, such as a scheduled run, receives `403`. Channel tasks run with the channel principal's permissions and always deliver to the channel; omit `data.delivery_channel` or pass the channel's ID, since any other value returns `422`.

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET` | `/api/v1/sandbox/scheduled_tasks` | List the linked user's tasks. |
| `GET` | `/api/v1/sandbox/scheduled_tasks/:id` | Read one owned task by `tsk_...` OID. |
| `POST` | `/api/v1/sandbox/scheduled_tasks` | Create a task from `data.name`, `data.prompt`, and optional `data.delivery_channel` (default `dm`), `data.cron_expression`, and `data.enabled`. |
| `PUT`/`PATCH` | `/api/v1/sandbox/scheduled_tasks/:id` | Update any supplied task fields. Set `data.cron_expression` to `null` or an empty string to make the task manual-only. |
| `DELETE` | `/api/v1/sandbox/scheduled_tasks/:id` | Delete an owned task. Returns `204`. |
| `POST` | `/api/v1/sandbox/scheduled_tasks/:id/run` | Queue an immediate run. Returns `202`; disabled tasks return `422`. |

The admin API also exposes `GET /api/v1/scheduled_tasks/:id` for scheduled workflows to read the task's current ID, enabled state, and delivery channel. It requires an active admin API key, returns `404` for deleted tasks, and sets `Cache-Control: no-store`.

Responses include the task's optional cron expression, fixed timezone, human-readable schedule, enabled state, next run time, and latest run metadata. The `centaur-console` CLI exposes the same operations through `tasks`, `task`, `create-task`, `update-task`, `delete-task`, and `run-task`. Omit `--cron` when creating a manual-only task, or pass `update-task --cron ""` to remove an existing recurring schedule.

### Console scheduled tasks

The console Scheduled Tasks pages use these endpoints. Any active Console user
may call them with the session cookie or a bearer API key. Each one is scoped
to tasks the caller authored. Any other user's task returns `404` for every
caller, admins included. Tasks owned by a Slack channel are managed from Slack
and are never returned here. Responses set `Cache-Control: no-store`.

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET` | `/api/v1/scheduled_tasks` | The caller's tasks, ordered by name. |
| `GET` | `/api/v1/scheduled_tasks/new` | Defaults for a new task: daily at 9:00 PT, enabled, and delivery to the caller's Slack DM when one is linked. |
| `GET` | `/api/v1/scheduled_tasks/:id?view=console` | One task in the console shape below. Without `view=console`, this is the admin execution-state lookup described above. |
| `POST` | `/api/v1/scheduled_tasks` | Create a task authored by the caller. Returns `201`. |
| `PUT`/`PATCH` | `/api/v1/scheduled_tasks/:id` | Update the fields that are sent and keep the rest. |
| `DELETE` | `/api/v1/scheduled_tasks/:id` | Returns `204`. |
| `POST` | `/api/v1/scheduled_tasks/:id/run` | Queue an immediate run. Returns `202` with the task and `queued: true`. A disabled task returns `422` with `details.reason=task_disabled` and the message `Enable the task before running it.` |
| `GET` | `/api/v1/scheduled_tasks/slack_channel_options` | Delivery channel search (`q`, at most 20). Returns public channels and the private channels that both the caller and the bot are in. When editing, `?task_id=` must be one of the caller's tasks; any other task returns `404`. The response shape matches `GET /api/v1/slack_channels`. `meta.catalog_error` can also report `Slack channel memberships are loading.` |

Write bodies take `data` with these fields:

| Field | Notes |
|---|---|
| `name`, `prompt`, `enabled` | Same validation as the sandbox endpoints. |
| `schedule_preset` | `manual`, `daily`, `weekdays`, `mondays`, `fridays` (all at 9:00 PT), `custom`, or `cron` (uses `cron_expression` verbatim). If omitted, the stored schedule is kept. |
| `custom_days`, `custom_time` | For `custom`: weekday values `"1"`..`"6"` and `"0"` (Sunday), plus `"HH:MM"`. A custom schedule without days or a time returns `422` on `cron_expression`. |
| `delivery_mode` | `dm` delivers to the author's linked Slack DM and ignores `delivery_channel`. `channel` uses `delivery_channel`. The server checks the destination against the author's Slack permissions and returns `422` on `delivery_channel` when it is not allowed. |

```json
{
  "data": {
    "id": "tsk_...", "name": "Daily support summary", "prompt": "...",
    "author_id": "usr_...", "principal_id": null,
    "delivery_channel": "C0123456789", "delivery_mode": "channel", "delivery_name": "#support",
    "direct_message_user_id": "U0123456789",
    "cron_expression": "0 9 * * *", "timezone": "America/Los_Angeles",
    "schedule_preset": "daily", "schedule_label": "Daily at 9:00 PT",
    "custom_days": ["0", "1", "2", "3", "4", "5", "6"], "custom_time": "09:00",
    "enabled": true, "next_run_at": "...", "last_enqueued_at": null,
    "last_run_id": null, "last_run_at": null, "last_error": null,
    "created_at": "...", "updated_at": "..."
  }
}
```

`delivery_name` is the channel's catalog name, or "Direct message to you" for a
DM. `direct_message_user_id` is the caller's linked Slack user ID, or `null`
when the caller has none.

## Workflows

The admin-only workflow dashboard reads the api-rs workflow read-only views.
Schedules and run details come from the workflows API and are best-effort.
`:id` is the workflow key: the workflow name, or the task name for older runs.
It may contain dots. If the workflow views are missing or unreadable, the
endpoints return `503` with `details.reason=workflow_database_unavailable`.
Responses set `Cache-Control: no-store`. Pages hold 50 rows, and `meta` is
`{ page, limit, total, total_pages }`.

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET` | `/api/v1/workflows` | The latest run of each workflow (`?page=`): `workflow_key`, `label`, `task_name`, `status`, `queue_name`, `queue_label`, `attempts`, `max_attempts`, `started_at`, `created_at`, `finished_at`, and `queues[]` (`queue_name`, `queue_label`, `status`, `last_activity_at`, `run_count`). No run or task IDs. |
| `GET` | `/api/v1/workflows/:id` | The latest run's fields, plus `harness_type`, `engine_label`, `duration_label`, `run_count`, `status_counts[]` (`status`, `count`, in tab order), `queues[]`, `schedule` (`label`, `summary`, `timezone`, `enabled`, `source_path`, `source_url`; `null` when unknown), and `debug[]` (`label`, `value`, `run_id`: Input, Result, Failure, Last failure). Returns `404` when the workflow has no runs. |
| `GET` | `/api/v1/workflows/:id/runs` | Runs, filtered by `?status=` and `?queue=`, plus `run_id` and `task_id`. |
| `POST` | `/api/v1/workflows/:id/runs` | Force-start a run with the first registered schedule's input. Returns `201` `{ run_id, created: true }`. If a run with the same idempotency key is already queued, returns `200` with `created: false`. A workflows API error returns `502` with `details.reason=workflow_api_error`. |

Debug values can include run input and results, so they are admin-only.

## Skills

Skills are mutable `SKILL.md` documents authored by signed-in users in Console. There is no revision or draft resource: updating a public skill changes the document returned to agents immediately. Skill IDs use the `skl_` prefix.

Console stores and validates `name`, `description`, and Markdown `instructions` as separate fields, then generates `SKILL.md`. Active skill names are globally unique, use lowercase letters, numbers, and hyphens, and are limited to 64 characters. The name `search` is reserved for the catalog search route. Because names cannot contain underscores, they cannot conflict with `skl_` OIDs. Archived names may be reused. Generated documents are limited to 64 KiB.

### Sandbox Operations

These endpoints use the existing sandbox entitlement JWT injected by `iron-proxy`. Principals linked to an active Console user can see that user's private skills, private skills they have been added to as an editor, and every public skill. Other principals can see public skills only. Mutations require an active Console user linked to the sandbox principal. Owners can perform every mutation on their skills, while editors can update skill content but cannot share, unshare, or archive the skill.

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET` | `/api/v1/sandbox/skills` | List visible skills. Optional `scope=private|shared` and `limit` up to 20. |
| `GET` | `/api/v1/sandbox/skills/search?q=...` | Full-text search visible skills. |
| `GET` | `/api/v1/sandbox/skills/:id` | Read a visible skill by its exact name or `skl_...` OID. |
| `POST` | `/api/v1/sandbox/skills` | Create a skill from `data.name`, `data.description`, and `data.instructions`. Optional `data.visibility` (`private` or `shared`) defaults to `shared`. |
| `PUT`/`PATCH` | `/api/v1/sandbox/skills/:id` | Update an owned or editable skill using name, description, and instructions (not visibility); optional `data.lock_version` detects concurrent edits. |
| `DELETE` | `/api/v1/sandbox/skills/:id` | Archive an owned skill. |
| `POST` | `/api/v1/sandbox/skills/:id/share` | Make an owned skill public. |
| `POST` | `/api/v1/sandbox/skills/:id/unshare` | Make an owned skill private. |
| `GET` | `/api/v1/sandbox/skills/:id/editors` | List editors for any visible skill by exact name or OID. |
| `POST` | `/api/v1/sandbox/skills/:id/editors` | Add an editor to an owned skill OID using an exact email or `usr_...` OID in `data.user`. |
| `DELETE` | `/api/v1/sandbox/skills/:id/editors` | Remove an editor from an owned skill OID using an exact email or `usr_...` OID in `data.user`. |

Editor responses include each editor's OID, email, name, and account status, plus the skill's current `lock_version`. The editor list follows skill visibility: every principal that can read a shared skill can also read its editor list, while a private skill's list remains visible only to its owner and editors. Adding an editor is idempotent. Catalog responses include the skill ID and a checksum over the generated document. Read responses set `Cache-Control: no-store`.

### Console skills

The Console Skills pages use these endpoints. They accept a bearer API key or a Console session cookie (writes need `X-CSRF-Token`) and are open to every **active** user, not only admins. Visibility matches the agent catalog: every public skill, plus the caller's own skills and the skills the caller edits. Requests for any other skill return `404`. Owners and editors can update content. Only the owner can manage editors, share, or archive. An acting admin (not descoped) can additionally make any public skill private, for moderation.

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET` | `/api/v1/skills` | List. `tab=directory` (default, active public skills) or `tab=mine` (skills the caller owns or edits). Optional `q` does a full-text search within the tab. Ordered by `updated_at` desc. No pagination. |
| `GET` | `/api/v1/skills/:id` | Read a visible skill by `skl_...` OID. |
| `POST` | `/api/v1/skills` | Create from `data.name`, `data.description`, `data.instructions`, and optional `data.editor_ids` (user OIDs). New skills are public. Returns `201`. |
| `PUT`/`PATCH` | `/api/v1/skills/:id` | Update an owned or editable skill. Send `data.lock_version` from the last read. A stale version returns `409` with `details.reason = "stale"`. `data.editor_ids` replaces the editor list for owners; editors' values are ignored, and omitting it keeps the current editors. |
| `DELETE` | `/api/v1/skills/:id` | Archive an owned skill. Returns `204`. The name becomes reusable. |
| `POST` | `/api/v1/skills/:id/share` | Make an owned skill public. |
| `POST` | `/api/v1/skills/:id/unshare` | Make an owned skill private. An acting admin can do this to any public skill. |
| `GET` | `/api/v1/skills/editor_candidates` | Active users other than the caller, ordered by email: `{id, name, email}`. Optional `q` filters by name or email. |

List rows contain `id`, `name`, `description`, `visibility` (`private` or `shared`), `author`, `owner` (`{id, name, email}`), `updated_at`, and `viewer.owner`. A single skill adds the following fields:

- `instructions`: the raw Markdown.
- `content_html`: the instructions rendered as a sanitized GitHub-flavoured subset. Every source character is HTML-escaped, only `http(s)` links are kept, and only the tags `a blockquote br code del div em h1-h4 li ol p pre strong table tbody td th thead tr ul` and the attributes `class href rel target` survive.
- `lock_version`, `checksum`, and `created_at`.
- `editors`: active editors, ordered by email.
- `viewer`: `{owner, can_edit, can_manage, can_unshare}`.

Validation failures return `422` with per-field `details`: `name`, `description`, `instructions`, and `editors`. For example, `editors: ["include an unavailable user"]` is returned for an unknown, inactive, or owner OID.

## Personal SSH key

`/api/v1/me/ssh_key` manages the **caller's own** personal SSH key, an `SshKeySecret` owned by the caller. It is open to every active user, using either a bearer key or a session cookie with `X-CSRF-Token` on writes. There is no ID in the path, so a caller can only ever reach their own key. Every response sets `Cache-Control: no-store`. The private key is write-only. No response contains it, and that includes validation errors.

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET` | `/api/v1/me/ssh_key` | The caller's key. When nothing has been saved yet, this returns `saved: false` with the default name `My SSH key` and null key fields. |
| `PUT`/`PATCH` | `/api/v1/me/ssh_key` | Save `data.name` and `data.private_key`. A blank or omitted `private_key` keeps the stored key. On success, the key is granted to the caller's own principals: their Console principal, plus their Slack DM principal when a verified Slack identity is linked. Saving again repairs missing grants. `422` returns `details.name` and `details.private_key`. |
| `DELETE` | `/api/v1/me/ssh_key` | Delete the key and every grant of it. Returns `204`, even when nothing was saved. |
| `POST` | `/api/v1/me/ssh_key/validate` | Check `data.private_key` without saving it. `200` returns `{valid: true, message, fingerprint}`. `422` returns `error.message` with `details.valid = false`. |

The key object contains these fields:

- `id`, `saved`, `name`, and `has_private_key`.
- `fingerprint`: SHA256.
- `public_key`: OpenSSH `type base64`.
- `created_at` and `updated_at`.
- `grants`: each entry is `{id, grantee_type: "role"|"principal", grantee_id, label, principal_kind}`.

Admins see personal keys as ordinary `ssh_key` secrets in [`GET /api/v1/secrets`](#secrets-all-kinds) and can grant them elsewhere.

## Proxies

A proxy represents an `iron-proxy` instance. It may be assigned a principal, in which case it receives config for the secrets granted to that principal. A proxy can also boot **unassigned**: it authenticates and syncs normally but receives an empty config until a principal is assigned. The principal can be assigned, swapped, or cleared at any time without reissuing the token.

A proxy's `status` is `assigned` when it currently holds a principal and `unassigned` otherwise. `principal_assigned_at` records when the current assignment was made (`null` while unassigned).

### Create

`POST /api/v1/proxies`

```json
{
  "data": {
    "name": "Edge Proxy - US",
    "principal_id": "prn_...",
    "labels": {
      "centaur.slack_user_id": "U0123456789",
      "centaur.slack_team_id": "T0123456789",
      "centaur.slack_channel_id": "C0123456789"
    }
  }
}
```

Returns `201`. The plaintext proxy `token` (`iprx_...`) is included **only** in this create response: save it immediately. The proxy uses it to authenticate to [proxy sync](#proxy-sync).

```json
{
  "data": {
    "id": "prx_...",
    "name": "Edge Proxy - US",
    "principal_id": "prn_...",
    "status": "assigned",
    "labels": {
      "centaur.slack_user_id": "U0123456789",
      "centaur.slack_team_id": "T0123456789",
      "centaur.slack_channel_id": "C0123456789"
    },
    "principal_assigned_at": "2026-06-01T10:00:00Z",
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

`name` is required. `principal_id` is optional: omit it to create an unassigned proxy (`status` is then `unassigned`, `principal_id` and `principal_assigned_at` are `null`). `labels` is optional and defaults to `{}`; keys and values must be strings. Labels initialized by Centaur's Slack API path use the `centaur.` prefix. When supplied, a missing principal returns `404`.

### Assign, swap, or clear the principal

`PATCH /api/v1/proxies/:id` (or `PUT`)

```json
{ "data": { "principal_id": "prn_..." } }
```

Assigns the principal when the proxy is unassigned, or swaps it when already assigned. The token is unchanged; the proxy picks up the new config on its next [sync](#proxy-sync). Send `"principal_id": null` to unassign. Omitting `principal_id` leaves the assignment unchanged; `name` and `labels` may also be updated. Omitting `labels` leaves labels unchanged, `"labels": null` clears labels, and an object replaces labels. A missing principal returns `404`. Returns `200` with the updated proxy.

### Other operations

| Method   | Path | Notes |
| -------- | ---- | ----- |
| `GET`    | `/api/v1/proxies` | List. Optional `principal_id` and `labels[k]=v` filters; paginated. Tokens are never returned. |
| `GET`    | `/api/v1/proxies/:id` | Fetch one (no token). |
| `DELETE` | `/api/v1/proxies/:id` | Deregister. Returns `204`. |

Deleting a principal does not delete its proxies: they become unassigned and can be reassigned.

## Proxy sync

`POST /api/v1/proxy/sync`

This endpoint is served by the separate `proxy-sync` service (`services/proxy-sync`), which reads iron-control's database directly; iron-control itself does not route it. The payload is documented here because it is assembled from the resources above.

Called by `iron-proxy` instances to fetch their configuration. **Authentication is the proxy bearer token** (`Authorization: Bearer iprx_...`), not an API key.

The proxy sends the config hash it currently holds. If it matches the freshly computed hash, the server returns only the hash so the proxy skips re-applying. Otherwise the full payload is returned.

Request:

```json
{ "config_hash": "sha256:0a1b2c3d..." }
```

`config_hash` is optional. It is an opaque, deterministic fingerprint of the config (the literal string `sha256:` followed by a hex digest); the proxy treats it as an ETag.

Response when the hash matches (no payload):

```json
{ "config_hash": "sha256:..." }
```

Response when the hash differs (full payload):

```json
{
  "config_hash": "sha256:...",
  "status": "assigned",
  "principal_id": "prn_...",
  "secrets": [
    {
      "source": { "type": "env", "var": "GITHUB_TOKEN" },
      "inject": { "header": "Authorization", "formatter": "Bearer {{ .Value }}" },
      "rules": [ { "host": "api.github.com", "methods": ["GET", "POST"], "paths": ["/repos/*"] } ]
    },
    {
      "source": { "type": "control_plane", "value": "s3cr3t" },
      "replace": { "proxy_value": "__DB_PASSWORD__" },
      "rules": [ { "host": "db.internal", "methods": ["*"] } ]
    }
  ],
  "transforms": [
    {
      "name": "gcp_auth",
      "config": {
        "keyfile": { "type": "aws_sm", "secret_id": "gcp-sa-keyfile", "region": "us-west-2" },
        "subject": "user@example.com",
        "scopes": ["https://www.googleapis.com/auth/cloud-platform"],
        "rules": [ { "host": "googleapis.com", "methods": ["*"], "paths": ["/v1/*"] } ]
      }
    },
    {
      "name": "hmac_sign",
      "config": {
        "credentials": { "secret": { "type": "aws_sm", "secret_id": "webhook-hmac-key", "region": "us-west-2" } },
        "timestamp": { "format": "unix_seconds" },
        "signature": {
          "algorithm": "sha256",
          "key_encoding": "hex",
          "output_encoding": "base64",
          "message": "{{ .Timestamp }}.{{ .Body }}"
        },
        "headers": [
          { "name": "X-Signature", "value": "{{ .Signature }}" },
          { "name": "X-Timestamp", "value": "{{ .Timestamp }}" }
        ],
        "rules": [ { "host": "hooks.example.com", "methods": ["POST"], "paths": ["/webhooks/*"] } ]
      }
    },
    {
      "name": "oauth_token",
      "config": {
        "tokens": [
          {
            "grant": "refresh_token",
            "token_endpoint": "https://slack.com/api/oauth.v2.access",
            "client_id": { "type": "env", "var": "SLACK_CLIENT_ID" },
            "refresh_token": { "type": "control_plane", "value": "xoxe-1-..." },
            "scopes": ["chat:write"],
            "header": "Authorization",
            "value_prefix": "Bearer",
            "rules": [ { "host": "slack.com", "methods": ["POST"], "paths": ["/api/*"] } ]
          }
        ]
      }
    }
  ],
  "postgres": [
    {
      "id": "pgs_...",
      "foreign_id": "analytics-pg",
      "dsn": { "type": "env", "var": "PG_ANALYTICS_DSN" },
      "database": "analytics",
      "role": "readonly",
      "settings": [ { "name": "app.tenant", "value": "centaur" } ]
    }
  ]
}
```

Notes on the proxy-sync payload, which differs from the REST representation:

- `status` is `assigned` or `unassigned`, and `principal_id` is the assigned principal (or `null`). An unassigned proxy gets a valid response with `status: "unassigned"` and empty `secrets`/`transforms`, which is distinct from an assigned proxy whose config is genuinely empty. These fields appear only in the full payload (not the hash-only response).
- The config hash incorporates the principal assignment, so assigning, swapping, or clearing the principal always changes the hash and the proxy refetches. A swap is a full replacement: the proxy should drop the previously delivered config rather than merge.
- The delivered config covers the proxy's principal's **effective grants**: secrets granted to the principal directly plus those granted to any [role](#roles) it holds. A secret reachable through more than one path appears once.
- `secrets` carries one entry per granted static secret that has a source (sourceless static secrets are skipped). `transforms` carries one `gcp_auth` transform per granted GCP auth secret, one `aws_auth` transform per granted AWS auth secret, one `hmac_sign` transform per granted HMAC secret, and a single bundled `oauth_token` transform whose `config.tokens` lists every granted OAuth token secret. An `hmac_sign` transform omits `allow_chunked_body` when it is `false`.
- `postgres` carries one entry per granted PG DSN secret, with the opaque `id` and `foreign_id` alongside it for sandbox env-var derivation and operator lookup. The proxy routes Postgres sessions by `database`; `role` is omitted when blank, as is `settings` when no session variables are configured.
- Each source is flattened: its `config` keys are merged up and tagged with `type` (the `source_type`). A `control_plane` source delivers its decrypted value inline as `value`.
- Rules use `methods` here, versus `http_methods` in the REST API. Blank rule fields are omitted.
- The top-level `rules`, `mcp`, and `ingest_token` fields the proxy also understands are intentionally omitted; iron-control has no models for them. Rules are carried per secret instead.
