# Token Broker

`centaur-token-broker` refreshes Console's broker credentials before their access
tokens expire. It works directly on Console's PostgreSQL schema: Console creates
and administers `broker_credentials`, and proxy-sync delivers their current
access tokens to iron-proxy.

Every poll interval it refreshes each due credential: one that is not dead,
whose `next_attempt_at` has passed (or is unset), and that has what its grant
needs (`refresh_token` credentials need a refresh token once they have been
refreshed). Each refresh holds the credential's row lock while it calls the
token endpoint, so a refresh token is never used twice at once, and a row
another refresh or a Console edit holds is skipped until the next poll.
Several replicas can run at once.

Grants:

- `refresh_token`: exchanges the stored refresh token. Without one, the
  credential is marked dead.
- `client_credentials`: requests a token with the client credentials.
- `password`: exchanges the stored refresh token, falling back to the username
  and password when there is none or the endpoint rejects it.
- `preqin`: like `password`, with Preqin's username and API key, as multipart
  forms. Refresh tokens go to the token endpoint's sibling `refresh_token`
  endpoint.

Credentials minted by Console's OAuth flows use their OAuth app's client
credentials and the provider's rules in
[`providers.json`](../../crates/centaur-token-broker/providers.json): whether
the refresh re-requests the granted scopes, and whether the client
authenticates in the form or with HTTP Basic authentication. A Console test fails until every
Console provider is listed there. A credential whose provider is not listed is
retried with backoff.

Each token request times out after the credential's `refresh_timeout_seconds`,
at most 60 seconds.

Outcomes:

- Success stores the access token, the refresh token if it was rotated, and the
  expiry (five minutes when the endpoint omits `expires_in`), and schedules the
  next refresh at the earlier of `expires_at` minus the larger of
  `early_refresh_slack_seconds` and `early_refresh_fraction` of the lifetime
  (at least 60 seconds), and `max_refresh_interval_seconds` after this refresh,
  but no sooner than a minute from now.
- Network failures, 5xx and 429 responses, other 4xx responses without an
  OAuth error, malformed successful responses, and credentials or apps missing
  or misconfiguring a value the request needs are retried with exponential
  backoff from 5 seconds to 5 minutes.
- Other responses with an OAuth error (RFC 6749 section 5.2, or Slack's
  `"ok": false`), and Preqin 4xx responses without one, mark the credential
  dead until it is re-authorized.
- Credentials token-broker cannot read (decryption fails, or
  `token_endpoint_headers` or `scopes` are malformed) are left unchanged, logged,
  and counted as `error` on every poll.

Successes and dead credentials bump the sync config cache version of the
principals the credential's token is granted to, so proxy-sync serves the
change. Logs identify credentials by ID and never include tokens, client
secrets, or response bodies.

## Metrics

`GET /metrics` exposes Prometheus metrics, and `GET /healthz` reports
liveness. The `provider` label is the credential's OAuth app provider (such as
`google` or `slack`), or for credentials without an app, the token endpoint's
host.

- `token_broker_refreshes_total{provider, grant, outcome}`: refresh attempts.
  `outcome` is `success`, `retry`, `dead`, or `error` (a database or
  decryption failure, labeled with provider and grant `unknown`).
- `token_broker_refresh_duration_seconds{provider, outcome}`: refresh attempt
  latency, including its token requests.
- `token_broker_token_requests_total{provider, result}`: token endpoint
  requests; a fallback makes two. `result` is `success`, `rejected` (a response
  that marks the credential dead), `rate_limited` (429), `server_error` (5xx),
  `redirect` (3xx, which is not followed), `client_error` (another 4xx),
  `malformed` (an unreadable successful response), `timeout`,
  `connection_failed`, or `network_error`.
- `token_broker_due_credentials`: credentials due at the start of the last poll.
- `token_broker_last_poll_timestamp_seconds`: when the last poll finished a
  pass over the due credentials (whatever their outcomes), or when the service
  started. Alert when it falls behind, which catches a poll that is stuck, for
  example on the database, or that keeps failing to list the due credentials.
- `token_broker_dead_credentials{provider}`: credentials that need
  re-authorization.
- `token_broker_backing_off_credentials{provider}`: live credentials whose last
  refresh failed and is being retried.

## Configuration

Every setting is available as a command-line option and an environment
variable; run `centaur-token-broker --help` for the complete list.

- `IRON_CONTROL_DATABASE_URL` — Console database server URL. It may include the database name.
- `IRON_CONTROL_DATABASE_NAME` — optional database name override.
- `IRON_CONTROL_AR_ENCRYPTION_PRIMARY_KEY` — Active Record encryption primary key.
- `IRON_CONTROL_AR_ENCRYPTION_KEY_DERIVATION_SALT` — Active Record key derivation salt.
- `TOKEN_BROKER_POLL_INTERVAL_SECONDS` — seconds between polls; defaults to `15`.
- `TOKEN_BROKER_CONCURRENCY` — credentials refreshed at once; defaults to `4`.
  Each holds a database connection during its token request, so keep it below
  `DATABASE_MAX_CONNECTIONS`.
- `DATABASE_MAX_CONNECTIONS` — SQLx pool limit; defaults to `5`.
- `BIND_ADDR` — listen address for `/healthz` and `/metrics`; defaults to `0.0.0.0:8080`.

On `SIGTERM` the service stops starting refreshes and finishes the ones in
progress, so a token the endpoint has just rotated is stored.

## Tests

Integration tests run refreshes against Postgres with Console's schema and a
local token endpoint. Load the schema into an empty database, then point the
tests at it:

```bash
cd services/console
RAILS_ENV=test CENTAUR_CONSOLE_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:55432/token_broker_test bin/rails db:prepare
cd ../..
TOKEN_BROKER_TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:55432/token_broker_test cargo test -p centaur-token-broker
```
