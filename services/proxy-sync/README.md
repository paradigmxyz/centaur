# Proxy Sync

`centaur-proxy-sync` serves `POST /api/v1/proxy/sync` for iron-proxy. It authenticates proxy bearer tokens and
builds effective proxy configuration directly from Console's PostgreSQL schema.
Console remains responsible for credential administration and token refresh.

It also serves `POST /api/v1/proxy/tenant_config` for the tenancy
`iron-proxy`. The proxy authenticates to that route with its control-plane
token and forwards the turn token it verified as `X-Iron-Tenant-Token`; proxy
sync verifies that token and builds one tenant's configuration from its
principal and requester claims. The route is disabled (404) unless
`CENTAUR_CODEMODE_SYNC_TOKEN` is set.

The Helm chart always deploys this service. `api-rs` sends administrative
requests to Console and configures managed proxies to poll this service through
`IRON_CONTROL_PROXY_SYNC_URL`. The tenancy proxy points
`IRON_TENANCY_CONFIG_URL` at the Code Mode route.

## Environment

- `IRON_CONTROL_DATABASE_URL` — Console database server URL. It may include the database name.
- `IRON_CONTROL_DATABASE_NAME` — optional database name override; the Helm chart sets this to Console's configured primary database.
- `IRON_CONTROL_AR_ENCRYPTION_PRIMARY_KEY` — Active Record encryption primary key.
- `IRON_CONTROL_AR_ENCRYPTION_KEY_DERIVATION_SALT` — Active Record key derivation salt.
- `CENTAUR_JWT_SIGNING_SECRET` — optional shared key for the single sandbox token.
- `CENTAUR_CODEMODE_SYNC_TOKEN` — optional bearer token the tenancy `iron-proxy` presents to `/api/v1/proxy/tenant_config`. Unset or empty disables the route.
- `CENTAUR_CODEMODE_TOKEN_PUBLIC_KEY` — SPKI PEM Ed25519 public key turn tokens are verified with. Without it the route refuses every tenant token.
- `CENTAUR_CONSOLE_URL` — Console URL. The Console host is an api-rs host, so the sandbox token is injected there too.
- `CENTAUR_API_URL` / `CENTAUR_API_SERVER_PROXY_HOSTS` — api-rs hosts the sandbox token is injected for.
- `BIND_ADDR` — listen address; defaults to `0.0.0.0:8080`.
- `DATABASE_MAX_CONNECTIONS` — SQLx pool limit; defaults to `5`.

## Code Mode principal config

```text
POST /api/v1/proxy/tenant_config
Authorization: Bearer <CENTAUR_CODEMODE_SYNC_TOKEN>
X-Iron-Tenant-Token: <turn token>
{"config_hash": "sha256:..." | ""}

200 {"config_hash"}
200 {"config_hash", "secrets", "transforms"}
401 wrong or missing control-plane token
403 missing, invalid, or expired turn token, or no configured public key
404 unknown principal, or the route is not configured
```

The turn token is an Ed25519 JWT minted by api-rs (`iss: centaur-api`,
`aud: centaur-codemode`). Its `sub` claim is the principal and its optional
`requester` claim is the requesting user on a shared-channel turn. The body and
response follow the tenancy `iron-proxy` control-plane contract: the proxy sends
its last known `config_hash`, proxy sync answers with only the hash when it is
unchanged, and otherwise returns the `secrets`/`transforms` from
`centaur_console_db::build_config` with proxy labels `{}`. `postgres` is
omitted because tenants do not open those tunnels. A `codemode` api-rs sandbox
token is appended as the final secret.

That token carries `binding: "codemode"`, an empty `proxy_id`, and
`sandbox_id: "codemode"`. It is minted from a per-principal ten-minute window
(`iat` = window start, `exp` = `iat + 900`) so repeated refreshes inside the
window produce the same config hash. api-rs re-checks the binding and the
delegated-admin rule on every request.

## Metrics

`GET /metrics` exposes Prometheus metrics for HTTP request rate, errors, latency,
in-flight requests, and sync cache hit/miss results. Requests to `/metrics` are
excluded from the HTTP request metrics.

## Tests

Integration tests run the HTTP handlers against Postgres with Console's schema.
Load the schema into an empty database, then point the tests at it:

```bash
cargo run -p centaur-console-db --example migrate -- postgresql://postgres:postgres@127.0.0.1:55432/proxy_sync_test
PROXY_SYNC_TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:55432/proxy_sync_test cargo test -p centaur-proxy-sync
```

Without `PROXY_SYNC_TEST_DATABASE_URL` they are skipped locally and fail in CI.
