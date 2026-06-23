# iron-control API

`iron-control` exposes a JSON API under `/api/v1`. Every resource endpoint requires API key authentication. The single exception is `POST /api/v1/proxy/sync`, which `iron-proxy` instances call with a proxy bearer token.

- [Authentication](#authentication)
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
- [Broker credentials](#broker-credentials)
- [Principals](#principals)
- [Roles](#roles)
- [Grants](#grants)
- [API keys](#api-keys)
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

`iron-proxy` instances authenticate to [`POST /api/v1/proxy/sync`](#proxy-sync) with their own token (`iprx_` followed by 64 lowercase hex characters), issued once when the proxy is created. An invalid proxy token returns `401` with `"invalid or missing proxy token"`.

## Conventions

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
- **Namespaced list filtering** (static secrets, GCP auth secrets, GCP ID token secrets, OAuth token secrets, principals, roles) requires a `namespace` query parameter and accepts an optional `labels[key]=value` filter that matches by JSONB containment (all supplied pairs must be present). Label values must be scalars.
- **Object IDs** are prefixed by type: `ssr_` (static secret), `gas_` (GCP auth secret), `gid_` (GCP ID token secret), `ots_` (OAuth token secret), `prn_` (principal), `role_` (role), `grant_` (grant), `ak_` (API key), `prx_` (proxy).
- **`namespace`** defaults to `"default"` when omitted on create. Once set, `namespace` and `foreign_id` are immutable.
- **`namespace` and `foreign_id`** must be URL-safe: only `A-Z a-z 0-9 - . _ ~`. `foreign_id` is optional and, when set, must be unique within its namespace. A `foreign_id` may not start with the resource's opaque-id prefix (e.g. `ssr_`), so it can never be mistaken for an OID.

### Upsert (`PUT` / `PATCH`)

For the resources with a `foreign_id` (static secrets, GCP auth secrets, GCP ID token secrets, OAuth token secrets, principals, roles), `PUT`/`PATCH /api/v1/<resource>/:id` is an **upsert**, and `:id` may be either an OID or a `foreign_id`:

- **`:id` is an OID** (it starts with the resource's prefix, e.g. `ssr_…`): updates that record. `404` if it does not exist — an OID is server-assigned, so it can't be created at a chosen value.
- **`:id` is anything else**: it is treated as a `foreign_id` within the body `namespace` (default `"default"`). The record is **updated if it exists, created if it does not**. Creation responds `201`; update responds `200`.

This makes provisioning idempotent: `PUT /api/v1/roles/infra` with `{"data":{"namespace":"acme", …}}` converges the `acme/infra` role whether or not it already exists, in one call. On the foreign-id form the namespace and foreign_id come from the URL/body, so omitting `foreign_id` from the body does not clear it.
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
| `token_broker`        | `credential_id`        | `credential_namespace`      | A managed [broker credential](#broker-credentials); see below. |

`control_plane` is special: the value is stored in iron-control itself. Supply it as a top-level `secret` field on the source (not inside `config`), and leave `config` empty:

```json
{
  "source_type": "control_plane",
  "secret": "the-actual-secret-value",
  "config": {}
}
```

The `secret` field is encrypted at rest, is write-only, and is never returned in any response. It is only permitted for `control_plane` sources; supplying it for any other type is a validation error, and omitting it for `control_plane` is also an error.

`token_broker` is also resolved by iron-control rather than by the proxy. `credential_id` names a [broker credential](#broker-credentials), and at sync time iron-control substitutes that credential's current access token, delivered inline exactly like a `control_plane` value. The reference never reaches the proxy. If the credential has no current token (it is still bootstrapping, or it is dead), the owning secret is omitted from the proxy's config until the credential recovers.

`credential_id` is either the credential's opaque id (`bcr_...`) or its `foreign_id`. With a `foreign_id`, `credential_namespace` is required; with an opaque id it must be omitted (opaque ids are namespace independent, so they can reference a credential in any namespace, including a shared one). The reference is validated on write: it must resolve to an existing broker credential.

```json
{ "source_type": "token_broker", "config": { "credential_id": "bcr_abc123" } }
```

```json
{ "source_type": "token_broker", "config": { "credential_id": "gmail", "credential_namespace": "acme" } }
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
| `namespace`      | optional    | Defaults to `"default"`. Immutable after create. |
| `foreign_id`     | optional    | Unique per namespace. Immutable after create. |
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
    "namespace": "default",
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
    "namespace": "default",
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
| `GET`  | `/api/v1/static_secrets?namespace=default` | List. `namespace` required; `labels[k]=v` and pagination optional. |
| `GET`  | `/api/v1/static_secrets/:id` | Fetch one. `404` if missing. |
| `GET`  | `/api/v1/static_secrets/lookup/:namespace/:foreign_id` | Fetch by namespace + foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/static_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. `source` and `rules` are replaced wholesale. |
| `DELETE` | `/api/v1/static_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's source, rules, and any grants that reference it are removed. The granted roles and principals are not deleted. |

## GCP auth secrets

A GCP auth secret mints short-lived GCP OAuth2 access tokens and injects them as `Authorization: Bearer`. It defines exactly one credential mechanism: either a `keyfile` [secret source](#secret-sources) (the service account JSON) or a `credentials_provider` (Application Default Credentials).

### Attributes

| Field                  | In requests | Notes |
| ---------------------- | ----------- | ----- |
| `namespace`            | optional    | Defaults to `"default"`. Immutable. |
| `foreign_id`           | optional    | Unique per namespace. Immutable. |
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
    "namespace": "default",
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
    "namespace": "default",
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
    "namespace": "default",
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
| `GET`  | `/api/v1/gcp_auth_secrets?namespace=default` | List. |
| `GET`  | `/api/v1/gcp_auth_secrets/:id` | Fetch one. |
| `GET`  | `/api/v1/gcp_auth_secrets/lookup/:namespace/:foreign_id` | Fetch by namespace + foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/gcp_auth_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. |
| `DELETE` | `/api/v1/gcp_auth_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's sources, rules, and any grants that reference it are removed. The granted roles and principals are not deleted. |

## GCP ID token secrets

A GCP ID token secret mints Google-signed OIDC ID tokens for an audience and injects them as a bearer header. It is used for private Cloud Run services, Cloud Run functions, IAP, API Gateway, and other Google audience-authenticated targets. It requires a service-account `keyfile` [secret source](#secret-sources) and at least one [rule](#request-rules).

### Attributes

| Field                 | In requests | Notes |
| --------------------- | ----------- | ----- |
| `namespace`           | optional    | Defaults to `"default"`. Immutable. |
| `foreign_id`          | optional    | Unique per namespace. Immutable. |
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
    "namespace": "default",
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
    "namespace": "default",
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
| `GET`  | `/api/v1/gcp_id_token_secrets?namespace=default` | List. |
| `GET`  | `/api/v1/gcp_id_token_secrets/:id` | Fetch one. |
| `GET`  | `/api/v1/gcp_id_token_secrets/lookup/:namespace/:foreign_id` | Fetch by namespace + foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/gcp_id_token_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. |
| `DELETE` | `/api/v1/gcp_id_token_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's source, rules, and any grants that reference it are removed. The granted roles and principals are not deleted. |

## AWS auth secrets

An AWS auth secret re-signs matching outbound requests with AWS SigV4. The workload's AWS SDK signs each request with throwaway placeholder credentials; `iron-proxy` strips that signature and re-signs with the real credentials, so the real keys never reach the workload. Each AWS credential is its own [secret source](#secret-sources): `access_key_id` and `secret_access_key` are required, and `session_token` is optional for temporary/STS credentials. `allowed_regions` and `allowed_services` scope what the proxy will sign for. At least one [rule](#request-rules) is required.

Each granted AWS auth secret is delivered to `iron-proxy` as its own `aws_auth` transform with its own rules (like a [GCP auth secret](#gcp-auth-secrets), and unlike OAuth token secrets, which are bundled).

### Attributes

| Field               | In requests | Notes |
| ------------------- | ----------- | ----- |
| `namespace`         | optional    | Defaults to `"default"`. Immutable. |
| `foreign_id`        | optional    | Unique per namespace. Immutable. |
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
    "namespace": "default",
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
    "namespace": "default",
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
| `GET`  | `/api/v1/aws_auth_secrets?namespace=default` | List. |
| `GET`  | `/api/v1/aws_auth_secrets/:id` | Fetch one. |
| `GET`  | `/api/v1/aws_auth_secrets/lookup/:namespace/:foreign_id` | Fetch by namespace + foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/aws_auth_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. Replaces the credential sources wholesale, so a `PUT` must resend `access_key_id` and `secret_access_key` (and `session_token`, to keep it). |
| `DELETE` | `/api/v1/aws_auth_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's sources, rules, and any grants that reference it are removed. The granted roles and principals are not deleted. |

## OAuth token secrets

An OAuth token secret mints OAuth2 access tokens for a single grant and injects them as a bearer header. Each credential field and each token-endpoint header is its own [secret source](#secret-sources). At least one [rule](#request-rules) is required.

### Attributes

| Field                    | In requests | Notes |
| ------------------------ | ----------- | ----- |
| `namespace`              | optional    | Defaults to `"default"`. Immutable. |
| `foreign_id`             | optional    | Unique per namespace. Immutable. |
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
    "namespace": "default",
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
    "namespace": "default",
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
| `GET`  | `/api/v1/oauth_token_secrets?namespace=default` | List. |
| `GET`  | `/api/v1/oauth_token_secrets/:id` | Fetch one. |
| `GET`  | `/api/v1/oauth_token_secrets/lookup/:namespace/:foreign_id` | Fetch by namespace + foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/oauth_token_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. |
| `DELETE` | `/api/v1/oauth_token_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's sources, rules, and any grants that reference it are removed. The granted roles and principals are not deleted. |

## PG DSN secrets

A PG DSN secret is a Postgres upstream credential: a connection string (DSN) resolved from a single secret [source](#secret-sources), plus an optional `SET ROLE` for the upstream session. It is delivered to `iron-proxy` with a required `foreign_id` and a required `database` for routing. The proxy multiplexes upstreams through one listener and routes by the Postgres database name the client sends. Multiple secrets may use the same `database` so different principals can route that database through different upstream roles. A principal's effective proxy config emits only one route per database, with higher-priority grants winning.

Listener and client knobs (bind address, client auth) are deliberately not modeled: they are proxy-host deployment concerns. There are no [request rules](#request-rules) either: a Postgres listener matches by port, not by request.

### Attributes

| Field         | In requests | Notes |
| ------------- | ----------- | ----- |
| `namespace`   | optional    | Defaults to `"default"`. Immutable after create. |
| `foreign_id`  | required    | Unique per namespace. Immutable after create. |
| `name`        | optional    | |
| `description` | optional    | |
| `labels`      | optional    | Object; defaults to `{}`. |
| `database`    | required    | Database name clients connect to through the proxy. Must match the upstream DSN's database. If several granted secrets use the same database, grant priority selects the effective route. |
| `role`        | optional    | Upstream `SET ROLE` applied to the session. |
| `settings`    | optional    | Ordered array of session variables (GUCs) the proxy SETs at session start, before the `SET ROLE`, and pins so clients cannot override them. Each entry is `{ "name", "value" }` for a literal value, or `{ "name", "value_from" }` to resolve the value from the assigned proxy principal at sync time (see [principal-derived values](#principal-derived-setting-values)). Names must be a bare or dotted identifier; `role` and `session_authorization` are reserved. Replaced wholesale on update. |
| `dsn`         | required    | A [secret source](#secret-sources) resolving to the connection string. Replaced wholesale on update. |

### Create

`POST /api/v1/pg_dsn_secrets`

```json
{
  "data": {
    "namespace": "default",
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
    "namespace": "default",
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

### Principal-derived setting values

A setting may take its value from the proxy's assigned principal instead of
storing a literal, by replacing `value` with `value_from`:

```json
{ "name": "centaur.slack_channel_id", "value_from": { "principal_label": "slack_channel_id" } }
```

`value_from` contains exactly one of:

| Key               | Resolves to |
| ----------------- | ----------- |
| `principal_label` | The named label on the assigned principal. A label the principal does not carry resolves to an empty string, so RLS-style policies fail closed. |
| `principal_field` | One of the principal's identity fields: `id` (the opaque `prn_...` id), `namespace`, `foreign_id`, or `name`. |

A setting has either `value` or `value_from`, never both; unknown
`principal_field` names and blank `principal_label` keys are rejected at create
and update time. References are resolved only in the proxy sync and
effective-config payloads; create, update, show, and list responses echo the
stored reference.

### Other operations

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/pg_dsn_secrets?namespace=default` | List. `namespace` required; `labels[k]=v` and pagination optional. |
| `GET`  | `/api/v1/pg_dsn_secrets/:id` | Fetch one. `404` if missing. |
| `GET`  | `/api/v1/pg_dsn_secrets/lookup/:namespace/:foreign_id` | Fetch by namespace + foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/pg_dsn_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. `dsn` is replaced wholesale. |
| `DELETE` | `/api/v1/pg_dsn_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's source and any grants that reference it are removed. The granted roles and principals are not deleted. |

## HMAC secrets

An HMAC secret signs matching outbound requests with an HMAC over a templated message and injects the signature (and any companion values) as request headers. The HMAC key is one [secret source](#secret-sources) under the required `secret` credential; additional named credentials are optional and available to the message and header templates as `.Credentials.<name>`. At least one [rule](#request-rules) is required.

Each granted HMAC secret is delivered to `iron-proxy` as its own `hmac_sign` transform with its own rules (like a [GCP auth secret](#gcp-auth-secrets), and unlike OAuth token secrets, which are bundled).

### Attributes

| Field                       | In requests | Notes |
| --------------------------- | ----------- | ----- |
| `namespace`                 | optional    | Defaults to `"default"`. Immutable. |
| `foreign_id`                | optional    | Unique per namespace. Immutable. |
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
    "namespace": "default",
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
    "namespace": "default",
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
| `GET`  | `/api/v1/hmac_secrets?namespace=default` | List. |
| `GET`  | `/api/v1/hmac_secrets/:id` | Fetch one. |
| `GET`  | `/api/v1/hmac_secrets/lookup/:namespace/:foreign_id` | Fetch by namespace + foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/hmac_secrets/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`; same body as create. |
| `DELETE` | `/api/v1/hmac_secrets/:id` | Delete. Returns `204`; `404` if missing. Cascades: the secret's sources, rules, and any grants that reference it are removed. The granted roles and principals are not deleted. |

## Broker credentials

A broker credential is an OAuth credential whose refresh-token lifecycle iron-control manages itself. iron-control runs the refresh loop, mints fresh access tokens before they expire, and delivers the current access token to `iron-proxy` inline through [proxy sync](#proxy-sync) wherever a [`token_broker` secret source](#secret-sources) references the credential by its `id`.

Unlike the secret types above, a broker credential is not granted directly and is not injected on its own. It is referenced by a `token_broker` source on a grantable secret (typically a [static secret](#static-secrets)), which carries the rules and injection config. The `refresh_token` never leaves iron-control.

The OAuth client credentials it refreshes with are fields on the credential, resolved by iron-control itself. `client_id` is not secret and is returned in responses; `client_secret` and the `token_endpoint_headers` values are encrypted at rest and never returned.

### Attributes

| Field                          | In requests | Notes |
| ------------------------------ | ----------- | ----- |
| `namespace`                    | optional    | Defaults to `"default"`. Immutable. |
| `foreign_id`                   | optional    | Unique per namespace. Immutable. |
| `name`, `description`          | optional    | |
| `labels`                       | optional    | |
| `token_endpoint`               | required    | OAuth token endpoint the refresh request is sent to. |
| `scopes`                       | optional    | Array of strings. |
| `client_id`                    | required    | OAuth client id. Returned in responses. |
| `client_secret`                | optional    | OAuth client secret. Write-only and encrypted at rest; omit for public clients. Never returned. |
| `token_endpoint_headers`       | optional    | Object mapping header name to a string value, sent on the refresh request. Values are write-only and encrypted; only the header names are returned (as `token_endpoint_header_names`). |
| `refresh_token`                | optional    | Write-only seed. Supplying a value (re)bootstraps the credential: it is scheduled to refresh immediately and any dead state is cleared. Never returned. |
| `early_refresh_slack_seconds`  | optional    | Refresh this many seconds before expiry. Defaults to `300`. |
| `early_refresh_fraction`       | optional    | Refresh once this fraction of the token's lifetime remains, when that is larger than the slack. In `[0, 1)`. Defaults to `0.2`. |
| `max_refresh_interval_seconds` | optional    | Refresh at least this often, even for long-lived tokens. Defaults to `86400`. |
| `refresh_timeout_seconds`      | optional    | Per-attempt timeout for the token endpoint request. Defaults to `30`. |

Read-only fields are returned but never accepted in requests:

| Field                         | Notes |
| ----------------------------- | ----- |
| `status`                      | `bootstrapping` (no token minted yet), `live`, or `dead` (an unrecoverable refresh failure; needs a new `refresh_token`). |
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

The minted `access_token`, the `refresh_token`, the `client_secret`, and the `token_endpoint_headers` values are never returned in any response.

Credentials minted by the [OAuth consent flow](#oauth-consent-flow) are linked to an OAuth app and delegate their `client_id` and `client_secret` to it: rotating the app's secret applies to every credential it minted. Such a credential needs no `client_id`/`client_secret` of its own, and its `scopes` reflect exactly what the IdP granted.

### Create

`POST /api/v1/broker_credentials`

```json
{
  "data": {
    "namespace": "default",
    "foreign_id": "gmail",
    "name": "Gmail",
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
    "namespace": "default",
    "foreign_id": "gmail",
    "name": "Gmail",
    "description": null,
    "labels": {},
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

When a refresh fails unrecoverably (for example the IdP returns `invalid_grant` because the refresh token was revoked), the credential's `status` becomes `dead` and it stops minting tokens. Supply a fresh `refresh_token` via `PUT` / `PATCH` to clear the dead state and reschedule it:

```json
{ "data": { "refresh_token": "1//0gNEW..." } }
```

### Other operations

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/broker_credentials?namespace=default` | List. `namespace` required; `labels[k]=v` and pagination optional. |
| `GET`  | `/api/v1/broker_credentials/:id` | Fetch one. `404` if missing. |
| `GET`  | `/api/v1/broker_credentials/lookup/:namespace/:foreign_id` | Fetch by namespace + foreign id. `404` if missing. |
| `PUT`/`PATCH` | `/api/v1/broker_credentials/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`. A `refresh_token` reseeds and clears dead state. Omitted fields are preserved; `client_secret` and `token_endpoint_headers` are only changed when supplied. |
| `DELETE` | `/api/v1/broker_credentials/:id` | Delete. Returns `204`; `404` if missing. Returns `409` if any `token_broker` secret source still references the credential (remove those references first). |

## OAuth apps

An OAuth app registers an OAuth client (provider, client id, and client secret) and the scopes its [consent flow](#oauth-consent-flow) requests. The app's whole identity is a globally-unique `slug`: a team member who knows the integration (for example `google`) opens `/oauth/<slug>/start`, consents, and each completed consent mints (or updates) a [broker credential](#broker-credentials) linked back to the app. The minted credential is refreshed by the normal broker loop and delegates its `client_id` / `client_secret` to the app.

Only managing the app's configuration requires API key auth. The consent flow endpoints themselves are unauthenticated and live on iron-control's own domain (see [OAuth consent flow](#oauth-consent-flow)).

Google and Slack are supported providers in this release. The `provider` field is validated against the supported set, so an unknown provider returns `422`.

### Attributes

| Field                  | In requests | Notes |
| ---------------------- | ----------- | ----- |
| `slug`                 | required    | The app's identity: globally-unique, URL-safe, and the name in the well-known consent links (`/oauth/<slug>/start`). Must not start with the opaque-id prefix. |
| `description`          | optional    | |
| `labels`               | optional    | |
| `provider`             | required    | The provider strategy. Currently `"google"` or `"slack"`. |
| `client_id`            | required    | OAuth client id. Not secret; returned in responses. |
| `client_secret`        | required on create | OAuth client secret. Write-only and encrypted at rest; on update it is only changed when supplied. Never returned. |
| `allowed_scopes`       | required    | Non-empty array of scope strings the start endpoint requests. A flow's optional `scopes` param must be a subset; omitting it requests all of these. |
| `credential_namespace` | optional    | Namespace for credentials minted by this app's flows. Defaults to `"default"`. |
| `enabled`              | optional    | Defaults to `true`. A disabled app rejects new consent flows; existing credentials keep refreshing. |

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
    "allowed_scopes": ["https://www.googleapis.com/auth/gmail.readonly"],
    "credential_namespace": "default"
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
    "credential_namespace": "default",
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

| Provider | `provider` value |
| -------- | ---------------- |
| Google   | `google`         |
| Slack    | `slack`          |

Slack OAuth apps should have token rotation enabled so the callback receives a refresh token for the broker refresh loop.
Slack OAuth apps should use normal Slack API scopes such as `channels:history`, not Sign in with Slack scopes such as `openid`, `email`, or `profile`.

## Principals

A principal is an identity (an application, service, or proxy owner) that can be granted secrets.

### Attributes

| Field        | In requests | Notes |
| ------------ | ----------- | ----- |
| `namespace`  | optional    | Defaults to `"default"`. Immutable. |
| `foreign_id` | optional    | Unique per namespace. Immutable. |
| `name`       | optional    | |
| `labels`     | optional    | |

### Operations

`POST /api/v1/principals`

```json
{ "data": { "namespace": "default", "foreign_id": "api-service", "name": "API Service", "labels": { "tier": "backend" } } }
```

Returns `201`:

```json
{
  "data": {
    "id": "prn_...",
    "namespace": "default",
    "foreign_id": "api-service",
    "name": "API Service",
    "labels": { "tier": "backend" },
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

| Method | Path | Notes |
| ------ | ---- | ----- |
| `GET`  | `/api/v1/principals?namespace=default` | List. |
| `GET`  | `/api/v1/principals/:id` | Fetch one by OID. To fetch by `foreign_id`, use the lookup route below. |
| `GET`  | `/api/v1/principals/lookup/:namespace/:foreign_id` | Fetch by namespace + foreign id. `404` if missing. |
| `GET`  | `/api/v1/principals/:id/effective_config` | [Effective config](#effective-config) the principal resolves to. `:id` is an OID. |
| `GET`  | `/api/v1/principals/lookup/:namespace/:foreign_id/effective_config` | [Effective config](#effective-config) by namespace + foreign id. `404` if missing. |
| `GET`  | `/api/v1/principals/:principal_id/grants` | [List the grants](#list-by-grantee) granted directly to the principal. |
| `PUT`/`PATCH` | `/api/v1/principals/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`. Only `name` and `labels` are mutable on an existing record; `namespace`/`foreign_id` apply only when creating. |

See [Role assignments](#role-assignments) for attaching roles to a principal.

### Effective config

`GET /api/v1/principals/:id/effective_config`
`GET /api/v1/principals/lookup/:namespace/:foreign_id/effective_config`

The config a principal resolves to, in the same shape `iron-proxy` receives on [proxy sync](#proxy-sync), for operator inspection. The principal is addressed by OID (`:id`) or by an explicit namespace + `foreign_id` via the lookup route.

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

A role is a reusable bundle of [grants](#grants). Principals are assigned roles, and a principal's effective secrets are the union of its own direct grants and the grants of every role it holds. Use a role to apply a common set of secrets (for example, shared infrastructure credentials) to many principals without re-granting each one.

Roles are namespaced. A principal may only be assigned roles in its own namespace.

### Attributes

| Field        | In requests | Notes |
| ------------ | ----------- | ----- |
| `namespace`  | optional    | Defaults to `"default"`. Immutable. |
| `foreign_id` | optional    | Unique per namespace. Immutable. Handy for idempotent provisioning. |
| `name`       | optional    | |
| `labels`     | optional    | |

### Operations

`POST /api/v1/roles`

```json
{ "data": { "namespace": "default", "foreign_id": "infra", "name": "Infra", "labels": { "kind": "shared" } } }
```

Returns `201`:

```json
{
  "data": {
    "id": "role_...",
    "namespace": "default",
    "foreign_id": "infra",
    "name": "Infra",
    "labels": { "kind": "shared" },
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

| Method   | Path | Notes |
| -------- | ---- | ----- |
| `GET`    | `/api/v1/roles?namespace=default` | List. `namespace` required; `labels[k]=v` and pagination optional. |
| `GET`    | `/api/v1/roles/:id` | Fetch one. |
| `GET`    | `/api/v1/roles/lookup/:namespace/:foreign_id` | Fetch by namespace + foreign id. `404` if missing. |
| `GET`    | `/api/v1/roles/:role_id/grants` | [List the grants](#list-by-grantee) attached to the role. |
| `PUT`/`PATCH` | `/api/v1/roles/:id` | [Upsert](#upsert-put--patch) by OID or `foreign_id`. Only `name` and `labels` are mutable on an existing record; `namespace`/`foreign_id` apply only when creating. |
| `DELETE` | `/api/v1/roles/:id` | Delete. Returns `204`. Cascades: the role's grants and its assignments are removed. |

### Role assignments

Assign and unassign roles on a principal. The assignment endpoints are nested under the principal; the role is identified by its OID.

`POST /api/v1/principals/:principal_id/roles`

```json
{ "data": { "role_id": "role_..." } }
```

Returns `201` with the assigned role's representation. Assigning a role from a different namespace, or one already assigned, returns `422`. An unknown principal or role returns `404`.

| Method   | Path | Notes |
| -------- | ---- | ----- |
| `GET`    | `/api/v1/principals/:principal_id/roles` | List the roles assigned to the principal. |
| `POST`   | `/api/v1/principals/:principal_id/roles` | Assign a role (`data: { role_id }`). |
| `DELETE` | `/api/v1/principals/:principal_id/roles/:id` | Unassign the role with OID `:id`. Returns `204`; `404` if not assigned. |

## Grants

A grant attaches exactly one secret to a **grantee** — either a principal or a [role](#roles). A principal receives a secret if it is granted directly or through any role the principal holds; its proxies then receive that secret through [proxy sync](#proxy-sync).

### Create

`POST /api/v1/grants` — supply exactly one grantee (`principal_id` **or** `role_id`) plus exactly one of `static_secret_id`, `gcp_auth_secret_id`, `aws_auth_secret_id`, `oauth_token_secret_id`, `pg_dsn_secret_id`, or `hmac_secret_id`:

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

Referencing a missing grantee or secret returns `404`. Supplying no grantee returns `422` with `"must reference one of principal_id, role_id"`; supplying no secret returns `422` with `"must reference one of static_secret_id, gcp_auth_secret_id, aws_auth_secret_id, oauth_token_secret_id, pg_dsn_secret_id, hmac_secret_id"`.

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
      "created_at": "2026-06-01T10:00:00Z",
      "updated_at": "2026-06-01T10:00:00Z"
    }
  ],
  "meta": { "page": 1, "limit": 50, "total": 1, "total_pages": 1 }
}
```

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
| `GET`    | `/api/v1/api_keys` | List your keys (paginated; no `namespace`). Tokens are never returned. |
| `GET`    | `/api/v1/api_keys/:id` | Fetch one (no token). |
| `DELETE` | `/api/v1/api_keys/:id` | Revoke (soft delete). Returns `204`. Revoking the key used for the current request returns `422` with `"cannot revoke the API key used for this request"`. |

## Proxies

A proxy represents an `iron-proxy` instance. It may be assigned a principal, in which case it receives config for the secrets granted to that principal. A proxy can also boot **unassigned**: it authenticates and syncs normally but receives an empty config until a principal is assigned. The principal can be assigned, swapped, or cleared at any time without reissuing the token.

A proxy's `status` is `assigned` when it currently holds a principal and `unassigned` otherwise. `principal_assigned_at` records when the current assignment was made (`null` while unassigned).

### Create

`POST /api/v1/proxies`

```json
{ "data": { "name": "Edge Proxy - US", "principal_id": "prn_..." } }
```

Returns `201`. The plaintext proxy `token` (`iprx_...`) is included **only** in this create response: save it immediately. The proxy uses it to authenticate to [proxy sync](#proxy-sync).

```json
{
  "data": {
    "id": "prx_...",
    "name": "Edge Proxy - US",
    "principal_id": "prn_...",
    "status": "assigned",
    "principal_assigned_at": "2026-06-01T10:00:00Z",
    "created_at": "2026-06-01T10:00:00Z",
    "updated_at": "2026-06-01T10:00:00Z"
  }
}
```

`name` is required. `principal_id` is optional: omit it to create an unassigned proxy (`status` is then `unassigned`, `principal_id` and `principal_assigned_at` are `null`). When supplied, a missing principal returns `404`.

### Assign, swap, or clear the principal

`PATCH /api/v1/proxies/:id` (or `PUT`)

```json
{ "data": { "principal_id": "prn_..." } }
```

Assigns the principal when the proxy is unassigned, or swaps it when already assigned. The token is unchanged; the proxy picks up the new config on its next [sync](#proxy-sync). Send `"principal_id": null` to unassign. Omitting `principal_id` leaves the assignment unchanged; `name` may also be updated. A missing principal returns `404`. Returns `200` with the updated proxy.

### Other operations

| Method   | Path | Notes |
| -------- | ---- | ----- |
| `GET`    | `/api/v1/proxies` | List. Optional `principal_id` filter; paginated. Tokens are never returned. |
| `GET`    | `/api/v1/proxies/:id` | Fetch one (no token). |
| `DELETE` | `/api/v1/proxies/:id` | Deregister. Returns `204`. |

Deleting a principal does not delete its proxies: they become unassigned and can be reassigned.

## Proxy sync

`POST /api/v1/proxy/sync`

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
