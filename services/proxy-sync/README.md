# Proxy Sync

`centaur-proxy-sync` is the Rust implementation of Console's
`POST /api/v1/proxy/sync` endpoint. It authenticates proxy bearer tokens and
builds effective proxy configuration directly from Console's PostgreSQL schema.
Console remains responsible for credential administration and token refresh.

The service is disabled by default. Enable it in the Helm chart with:

```yaml
proxySync:
  enabled: true
```

When enabled, `api-rs` continues sending administrative requests to Console but
configures newly created managed proxies to poll this service.

## Metrics

`GET /metrics` exposes Prometheus metrics for HTTP request rate, errors, latency,
in-flight requests, and sync cache hit/miss results. Requests to `/metrics` are
excluded from the HTTP request metrics.

## Environment

- `IRON_CONTROL_DATABASE_URL` — Console database server URL. It may include the database name.
- `IRON_CONTROL_DATABASE_NAME` — optional database name override; the Helm chart sets this to Console's configured primary database.
- `IRON_CONTROL_AR_ENCRYPTION_PRIMARY_KEY` — Active Record encryption primary key.
- `IRON_CONTROL_AR_ENCRYPTION_KEY_DERIVATION_SALT` — Active Record key derivation salt.
- `CENTAUR_JWT_SIGNING_SECRET` — optional shared key for generated API and sandbox-entitlement credentials.
- `CENTAUR_CONSOLE_URL` — Console URL used for sandbox-entitlement rules.
- `CENTAUR_API_URL` / `CENTAUR_API_SERVER_PROXY_HOSTS` — API hosts used for generated API credentials.
- `BIND_ADDR` — listen address; defaults to `0.0.0.0:8080`.
- `DATABASE_MAX_CONNECTIONS` — SQLx pool limit; defaults to `5`.
