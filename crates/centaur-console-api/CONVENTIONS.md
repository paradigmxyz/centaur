# Console API conventions

How code in `centaur-console-api` (and `centaur-console-auth`, plus
`centaur_console_db::{schedule, keys, sync_config}`) is written. Read RFC 0006
(Decisions 4 and 5) first. `src/identity/` is the reference area: copy its
shape.

The goal is small, obvious code on standard crates and plain SQL. The
contract other software depends on (Decision 5) is: paths, methods, status
codes, JSON field names and types, the error envelope, auth rules, side
effects, the database schema and rows, and JWT claims. Before you change any
response, grep its callers: the SPA (`services/console/web/src`),
`tools/infra/centaur-console`, `crates/centaur-iron-control`,
`crates/centaur-scheduled-tasks`, the tools, and `e2e/tests`.

`centaur-console` (`main.rs`) merges every area's `routes()` and hands them
to `app::router`. A new area adds its `routes()` there.

## Routes

An area exposes one function returning a `Routes` value: a plain axum
`Router<AppState>` per credential namespace. Paths are **relative to the
namespace prefix**, which `app` adds together with the namespace's guard.
An area cannot mount a handler under the wrong credential or forget the check.

```rust
pub fn routes() -> Routes {
    Routes {
        bearer: Router::new()
            .route("/roles", get(list).post(create))
            .route("/roles/{id}", get(show).patch(update).delete(destroy)),
        console: Router::new().route("/roles", get(console::list)),
        ..Routes::default()
    }
}
```

| `Routes` field | Prefix | Credential | Handler extractor |
| --- | --- | --- | --- |
| `bearer` | `/api/v1` | `iak_` API key of an active admin | `BearerCaller` |
| `console` | `/api/v1/console` | console session, active **admin** | `ConsoleCaller` |
| `console_active` | `/api/v1/console` | console session, any active user | `ConsoleCaller` |
| `console_pending` | `/api/v1/console` | console session, pending users too | `ConsoleCaller` |
| `sandbox` | `/api/v1/sandbox` | entitlement JWT bound to its proxy and principal | `SandboxCaller` |
| `sandbox_claims` | `/api/v1/sandbox` | entitlement JWT signature only (no DB) | `SandboxClaimsCaller` |
| `sandbox_admin` | `/api/v1/sandbox/admin` | sandbox of an active admin's Slack DM | `SandboxAdminCaller` |
| `internal` | `/internal` | api-rs service token (errors are flat `{"error": "..."}`) | `InternalCaller` |
| `public` | none | none: pages and protocol endpoints authenticate themselves | `Session` |

- Console routes are admin-only unless you put them in `console_active` /
  `console_pending`. Scope what you load to the caller in `console_active`
  handlers (`ConsoleCaller(user)`; `user.admin` tells you if they may see all).
- A route that sits in the wrong field fails closed: its extractor finds no
  credential and answers 401 `credential_namespace_mismatch`.
- Path parameters use axum 0.8 syntax: `/roles/{id}`. There are no `.json`
  suffixes. Unknown paths under `/api`, `/internal`, `/mcp`, `/oauth`, ... are
  a JSON 404; browser navigations elsewhere get the SPA shell.
- Public pages sit under `/login`, `/oauth/...`, `/mcp/...`, `/auth/...`
  (listed in `assets::NOT_SPA`; add a prefix there if you add a new top-level
  protocol path).
- Do not add `.layer(...)` for credentials, sessions, CSRF, security headers,
  or metrics in an area; `app` applies them.

## Extractors

Use the typed extractors in `extract`; their rejections are `ApiError`s
(JSON envelope), never axum's plain-text bodies.

```rust
async fn create(
    State(state): State<AppState>,
    BearerCaller(auth): BearerCaller,
    Data(input): Data<NewRole>,        // bearer request body: { "data": {...} }
) -> ApiResult { ... }

async fn update(Path(id): Path<String>, ConsoleCaller(user): ConsoleCaller, Json(input): Json<Patch>) -> ApiResult
async fn index(Query(filter): Query<Filter>, Query(page): Query<PageParams>) -> ApiResult
async fn login(Form(form): Form<LoginForm>) -> ApiResult    // server-rendered pages
```

- Input structs derive `Deserialize`; use `Option<T>` for optional fields and
  `#[serde(default)]` for optional collections. Do not hand-parse `serde_json::Value`
  unless the field is genuinely free-form (secret `config`, labels).
- Do not emulate Rails strong parameters or `a[]=` parsing. If a caller
  sends something else, fix the caller or accept both explicitly.
- Path ids are oids (`oid("prn", id)`); decode with `decode_oid("prn", &id)
  .ok_or_else(ApiError::not_found)?`. A malformed oid is a 404.
- Lists: `pagination::{PageParams, Page}` (`page`/`limit`, default 50, max
  200, `meta` with `total_pages`). Extract it as its own `Query<PageParams>`
  (serde cannot combine `flatten` with numeric query values).
- Responses: `data(value)` gives `{"data": ...}`. Timestamps are RFC 3339 UTC
  (`DateTime<Utc>` with serde); do not format them by hand.

## Errors

`ApiError` is the only error type; `ApiResult<T = Response>` the return
type. It renders `{"error":{"message":..,"details":..}}`:

| Variant | Status | Use |
| --- | --- | --- |
| `BadRequest(msg)` | 400 | malformed request (also what extractors return) |
| `Denied(Denial)` | 401/403 | credential guards (+ `details.reason`) |
| `Forbidden(msg)` | 403 | authenticated, not allowed (ownership) |
| `NotFound(msg)` / `ApiError::not_found()` | 404 | unknown or not yours (prefer 404 over 403 when existence is private) |
| `Conflict(msg)` | 409 | state conflicts |
| `Invalid(FieldErrors)` / `ApiError::field(f, m)` | 422 | validation: `details: {field: [messages]}` |
| `Unprocessable { message, reason }` | 422 | a rule that is not about one field |
| `Internal(msg)` | 500 | logged, never returned |

- Validate in the handler (or a small `fn validate(&Input) -> FieldErrors`),
  collecting into `FieldErrors`, then `errors.finish()?`. Messages are free text (`"can't be blank"`, `"is required"`),
  but keep the field names stable: the SPA shows them per field.
- Unique violations: do the insert and map the database error:
  `.map_err(|e| ApiError::unique(e, "name", "has already been taken"))?`
  instead of a check-then-insert race.
- `sqlx::Error` converts with `?` to a 500. Never put secrets or SQL in
  error messages; `Internal` logs its message, so keep it free of values.

## SQL style

Plain `sqlx` with `query` / `query_as` (the offline build has no
`query!`). No ORM, no generic record/table layer, no `Record`/`OidRecord`.

- One `FromRow` struct per use case that selects **only the columns it needs**,
  declared next to the handler. Share a struct only when two handlers truly
  need the same columns.
- `UPDATE` sets only the columns the request changes. Never load a row, edit a
  copy, and write every column back: a concurrent rotation or revocation would
  be silently undone (this was a review finding). When several optional fields
  can change, build the `SET` list from the fields present, or use
  `COALESCE($n, column)` for plain optional columns.
- Timestamps: columns are `timestamp` (UTC, no zone). Bind `now.naive_utc()`
  (`state.now()` is the injected clock), and select
  `col AT TIME ZONE 'UTC' AS col` into a `DateTime<Utc>` for output.
- Transactions are explicit: `let mut tx = state.db.begin().await?; ...;
  tx.commit().await?;`. Anything that must happen only once the write is durable (calling api-rs,
  Slack, background work) is plain code after `tx.commit()`, and its failure
  policy (ignore and log, or return) is written at the call site.
- Row locks where a check-then-write must be atomic: `SELECT ... FOR UPDATE` inside the
  transaction, then re-check the state you locked for.
- oids: `centaur_console_db::{oid, decode_oid}` with the table's prefix
  (`usr`, `prn`, `prx`, ...).
- Child rows and cascades are the schema's job (`ON DELETE`), not a loop.

## Encryption and secrets

- Encrypted columns (`oauth_apps.client_secret`, `secret_sources.secret`, the
  broker credential columns) use the existing ActiveRecord format through
  `state.encryptor` (`Encryptor::encrypt` / `decrypt`, `Ciphertext` binds as
  text). Never re-encrypt rows that are only being read.
- Decrypt only where the plaintext is used. Never return, log, or put
  plaintext in an error. Types that hold secrets implement `Debug` by hand and
  redact (`centaur_console_db::redact::Redacted`), including request/response
  structs that carry them; tests assert the sentinel does not appear.
- Password hashes: `centaur_console_auth::password::{hash, verify}` (async,
  run on the blocking pool with bounded concurrency; never call `bcrypt`
  directly in a handler).
- Token hashes: SHA-256 hex (`bearer::hash_token`); compare secrets with
  `centaur_console_auth::csrf::eq` (constant time).

## Sessions, CSRF, redirects

- Browser requests carry one encrypted session cookie (`_centaur_console`):
  user id, issue time, CSRF secret, one-shot flash and return path. It is
  HttpOnly, SameSite=Lax, Secure over HTTPS, and expires 14 days after
  issue; sign-in issues a new session (`session.sign_in`).
- Console API writes need `X-CSRF-Token` (the `csrf_token` from
  `GET /api/v1/console/me`) and a same-origin `Origin`; the console guard
  enforces both, and a write without `Origin` is refused (browsers always send
  it, and it is the only way to tell where a cookie-bearing write came from). Server-rendered form posts call
  `auth::verify_form_csrf(&state, &session, &headers, form.authenticity_token)`;
  forms include `<input type="hidden" name="authenticity_token" value="{{ csrf }}">`.
- `Session` (extractor) is available in `public` routes and console routes.
  `session.lock()` returns a guard: use it briefly and never hold it across
  an `.await`.
- OAuth/SSO `state` values: `state.signer.sign(purpose, &data, ttl, now)` /
  `verify(purpose, token, now)`; bind the flow to the browser with
  `state.flow_cookies` (see `identity/sso.rs`).
- Redirect targets that come from a request go through
  `centaur_console_auth::return_path::safe`. Redirects use
  `pages::redirect` (302).
- Server-rendered pages: askama templates under `templates/pages/` extending
  `pages/layout.html`, rendered with `pages::render` (strict CSP: no script,
  no inline style). The page struct needs an `assets: &Assets` field for the
  layout.

## Outbound HTTP and JWTs

- `state.http` is the one `reqwest::Client` (proxies from the environment,
  `SSL_CERT_FILE` roots, no redirects, 10 s timeout). Do not build another.
  Log errors with `http::without_url(error)`; a URL may carry credentials or
  authorization codes.
- Limit response sizes you parse from third parties (`bytes().await` then
  check the length) and never log bodies.
- JWTs are minted with `centaur_console_auth::tokens` (HS256,
  `jsonwebtoken`): `api_principal`, `console_service`, `sandbox_entitlement`,
  `internal_service`, `mcp_access`. Claims are a contract with api-rs,
  iron-proxy, and proxy-sync; do not change them.
- Time comes from `state.now()`, never `Utc::now()` in handlers (tests fix
  the clock).

## Sync-config invalidation

proxy-sync re-reads a principal's config when `principals.sync_config_cache_version`
changes. Any write that can change what a principal's config contains (grants,
roles and role members, secrets and their sources, OAuth app/credential state,
system settings, ...) must bump it. In the writing transaction, after the last
write and just before `commit`:

```rust
use centaur_console_db::sync_config::{bump_sync_config, grantee_ids, role_member_ids, bump_all_sync_config};

let affected = grantee_ids(&mut tx, "static_secret_id", secret_id).await?; // directly or via roles
bump_sync_config(&mut tx, &affected).await?;
tx.commit().await?;
```

`grantee_ids` (principals granted a grantable, directly or through a role),
`role_member_ids`, and `bump_all_sync_config` (system-wide settings) cover the
cases. Resolve the ids **after** your
writes (a grant that was just removed should still invalidate the principal
that lost it, so collect the ids before deleting grants and after creating
them). `updated_at` is not touched. Add a test per write path that asserts the
version moved.

## Cron, SSH keys, CIDR, JSON Schema, Markdown

Standard crates behind tiny modules; do not re-implement any of them:

- schedules: `centaur_console_db::schedule` (`croner`; `next_run`,
  `is_valid`, `schedule_error`, presets, `weekly`, `label`, `cron_for_choice`).
  Shared with `centaur-scheduled-tasks`.
- SSH private keys: `centaur_console_db::keys::inspect` (`ssh-key`; validates,
  derives public key and `SHA256:` fingerprint, user-facing `KeyError`s).
- CIDR: `support::cidr` (`ipnet`). JSON Schema: `support::json_schema`
  (`jsonschema`). Markdown: `support::markdown` (`pulldown-cmark` +
  `ammonia`).

## Tests

DB tests use a database cloned from a migrated template
(`centaur_console_db::testing`); they skip when
`CONSOLE_DB_TEST_DATABASE_URL` is unset (and fail under `CI`). Run your own
Postgres: `docker run -d --name <you>-pg --shm-size=1g -e POSTGRES_PASSWORD=postgres
-p <port>:5432 postgres:16 -c max_connections=300` and
`export CONSOLE_DB_TEST_DATABASE_URL=postgres://postgres:postgres@localhost:<port>/postgres`.

```rust
let Some(app) = TestApp::new(my_area::routes()).await else { return };
```

(`testing`, feature `testing`.) Fixtures are `create_user`,
`create_api_key`, `create_principal`, `create_proxy`, `sandbox_token`,
`internal_token`; everything else is SQL against `app.db.pool`. `app.client()`
is a browser (cookies kept; `sign_in(user_id)` starts a signed-in session
whose CSRF token `.csrf(&browser)` sends).

Write ordinary behaviour tests:

1. **An authorization matrix per endpoint**: `app.assert_console_access(Access::Admin,
   Method::POST, "/api/v1/console/things", Some(&json!({}))).await`, and
   `assert_bearer_access` for bearer routes (anonymous, wrong credential,
   disabled, pending, non-admin, CSRF/Origin for writes). For sandbox and internal
   routes assert missing/forged/other-namespace tokens by hand (see
   `tests/base.rs`).
2. Ownership scoping for anything a non-admin can reach: another user's rows
   are 404.
3. Happy path with the exact JSON the callers read (field names and types).
4. Validation failures (422 + `details`), unique violations, 404s for
   unknown/malformed ids.
5. Side effects: rows written, `sync_config_cache_version` bumped, calls to
   api-rs/Slack/IdPs (stub with a local axum server; see `tests/sso.rs`),
   and nothing happening when the request is refused.
6. Secret hygiene: plaintext never in a response, an error, or `Debug` output.
7. A regression test for every bug you fix.

Do not write golden byte-for-byte responses.

## What not to do

Do not reintroduce Rails/Ruby emulation: Rails routing `(.:format)` and format negotiation, strong params and `a[]=`
parsing, ActiveModel error ordering and texts, Rack ETag/conditional/range
handling (use `tower-http`), Ruby integer/URI/IPAddr/YAML/regex semantics,
fugit, Net::HTTP quirks, Rails JSON escaping and key-order preservation,
Rails timestamp formats, browser allow-listing, Rails public error HTML
pages, `// Parity:` quirks (remove the quirk **and** the comment), the
generic record/table layer (`Record`, `OidRecord`, `tables::*` structs that
write every column), `ordered_json!`/`JsonValue` where a typed struct or
`serde_json::Value` will do, custom cookie/`MessageEncryptor`/`MessageVerifier`
code, the per-request `RequestContext`, and the namespace state types.
