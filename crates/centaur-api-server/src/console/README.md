# Rust Console layer

This tree serves the Console: the session cookie, the unified
credential/capability guard, the shared JSON/error helpers, the server-rendered
pages, the static SPA files, and every Console area's routes. New routes go
through the builder here; they do not reimplement authentication. The SPA and
the API reference live in `services/console/` (`web/`, `docs/API.md`).

The layer is mounted only when **both** `CONSOLE_SESSION_SECRET` and
`CENTAUR_CONSOLE_PUBLIC_URL` are configured. `CENTAUR_CONSOLE_PUBLIC_URL`
without a secret leaves it disabled; a secret without a URL is a startup error.
When disabled, these paths fall through to the rest of api-rs exactly as before.

## Layout

| File | Responsibility |
| --- | --- |
| `mod.rs` | Mount point, environment switches, JSON 404 fallback, host routing. |
| `host.rs` | Console-host vs internal-host path split (see "Host routing"). |
| `guard.rs` | The single route builder and authorization layer (see "Authorization"). |
| `session_cookie.rs` | AES-256-GCM stateless session cookie. |
| `return_path.rs` | Same-origin check for post-login redirects. |
| `http.rs` | Error envelope, JSON 404, `{"data": ...}` envelopes, pagination, label filters, lists, origin. |
| `request.rs` | Typed `data`/body/query/form extractors and their validation errors. |
| `assets.rs` | Embedded `services/console/public/assets/oauth-page` files. |
| `static_files.rs` | Static mount: the SPA shell, `/spa/assets/*`, and root public files. No session or DB access. |
| `pages.rs` | Server-rendered `/login`, `/pending`, `/logout`. |
| `tests_db.rs` | DB-backed integration tests for the shipped routers. |

`console_session.rs` (one level up) owns `ConsoleSessionConfig` and the
`/me`/`/session` shell endpoints. `mod.rs`, `sandbox.rs`, and `guard.rs` share
the single authorization model described below; `mod.rs` mounts the bearer API
at `/api/v1`, and `sandbox.rs` owns the sandbox token verification and live
binding checks.

## Host routing

The Console and api-rs share one listener. `host.rs` compares the request
`Host` with the host (and effective port) of `CENTAUR_CONSOLE_PUBLIC_URL`,
case-insensitively. It reads `Host` only; api-rs also uses `X-Forwarded-Host`
when it builds absolute OAuth metadata URLs, but that header never selects the
router. A missing or unparseable `Host` is not the Console host.

Host routing is not where authorization happens. The unified guard in the next
section still verifies every credential and decides access. The one extra
routing rule is that the cookie console API is not exposed on an internal host:
a `/api/v1/console/*` request without an `Authorization` header is a 404 before
the guard runs. A delegated JWT carries `Authorization`, so it reaches the same
guard as on the public host.

| Surface | Paths | Credential | Console host | Any other host |
| --- | --- | --- | --- | --- |
| SPA shell, root public files | `/`, `/console/*`, other browser navigations, `/icon.svg` | Console session cookie when signed in | yes | 404 |
| Server-rendered pages | `/login`, `/logout`, `/pending`, `/auth/*`, `/oauth/:slug/{start,callback}`, `/mcp/oauth/*`, `/.well-known/oauth-authorization-server`, `/.well-known/openid-configuration` | form/session cookie | yes | 404 |
| OAuth page assets | `/assets/oauth-page/*` | none | yes | 404 |
| Console API | `/api/v1/console/*` | `console.*` capability: session cookie, delegated sandbox token, or another credential that holds it | yes | credential-bearing requests only; a cookie-only request is 404 |
| Bearer API | the rest of `/api/v1/*` | `operator.api`: Console API key whose owner is an active admin | yes | yes |
| Sandbox API | `/api/v1/sandbox/*` | `sandbox.*` capability from the single sandbox token | yes | yes |
| api-rs session, sandbox, Slack, workflow, scheduled-task, admin, and webhook routes | `/api/session/*`, `/api/sandboxes/*`, `/api/slack/*`, `/api/workflows/*`, `/api/scheduled-tasks/*`, `/api/admin/*`, `/api/webhooks/*` | the capability each route declares | 404 | yes |
| Health and metrics | `/healthz`, `/readyz`, `/metrics` | none | 404 | yes |
| MCP server and protected-resource metadata | `/mcp`, `/.well-known/oauth-protected-resource[/mcp]` | MCP auth | 404 | yes |
| API-client token issuance | `/oauth/token` | API client | 404 | yes |

Consequences worth keeping:

- In-cluster services (iron-proxy, bots, `centaur-perms`, the connectors) reach
the Console API through the internal Service name, so they get `/api/v1/*` on a
non-Console host. Browser sessions do not: the SPA shell, server pages, and a
cookie-authenticated `/api/v1/console/*` answer 404 there. A sandbox token still
works on both hosts because it carries its own `Authorization` header and no
cookie.
- The Kubernetes probes connect to the pod IP, not the public Console host, so
`/healthz` and `/readyz` being absent on the Console host is intentional. A
reverse proxy in front of the public host must not retarget probes at it. The
e2e stack probes `/login` on the Console host for readiness for the same
reason.
- When the Console layer is disabled (no `CONSOLE_SESSION_SECRET`), no host
middleware is mounted and the listener serves exactly the api-rs routes it did
before the port.

## Authorization

Every credential resolves to an `AuthenticatedCaller`: an identity plus a set
of `auth::Capability` values. Every route declares the one capability it
requires. `console::guard::GuardedRoutes` is the only route builder and the
only enforcement layer, replacing the old `ConsoleRoutes`, `BearerRoutes`, and
`SandboxRoutes` guards and api-rs's `route_access` table. There is no unguarded
`Router::route` path for an authenticated API namespace.

The route's capability selects the unauthenticated/forbidden error envelope
(`Capability::surface`). The path does **not** select which credential type is
accepted: on any guarded route the guard tries the single sandbox token, a
Console API key, api-rs's static/service credentials, and (on a Console route,
when no `Authorization` header is present) the Console session cookie.

### Capability catalog

| Capability | Grants | Surface |
| --- | --- | --- |
| `sessions.read` / `sessions.write` | api-rs session reads/writes | api-rs |
| `sessions.sandbox_release` | `DELETE /api/session/:key/sandbox` (API clients and service credentials; never an ingress key) | api-rs |
| `sandboxes.drain` | sandbox drain and session pause | api-rs |
| `workflows.read` / `workflows.write` / `workflows.events` / `workflows.actions` | the workflow, scheduled-task, and Slack-button routes | api-rs |
| `slack.proxy` | `/api/slack/*`, scoped by the sandbox token's channel lists | api-rs |
| `slack_external_teams.read` / `.write` | the Slack Connect allowlist | api-rs |
| `console.read` | any enabled Console account | Console |
| `console.pending` | `/me` and `/session`, including pending accounts | Console |
| `console.user` | skills, scheduled tasks, personal SSH key, integrations for an active user | Console |
| `console.admin` | everything else under `/api/v1/console` | Console |
| `operator.api` | `/api/v1/*` outside the console and sandbox namespaces | Bearer |
| `sandbox.permissions` / `sandbox.skills` / `sandbox.tasks` / `sandbox.oauth_apps` | the matching `/api/v1/sandbox/*` resources | Sandbox |
| `console.delegate` | a sandbox credential acting as its linked active Console admin | Console |

`console.admin` implies `console.user`, `console.user` implies
`console.pending`, and `console.pending` implies `console.read`;
`console.delegate` implies every Console level. Nothing else implies another
capability.

### Credential -> capability

| Credential | Identity | Capabilities |
| --- | --- | --- |
| Console session cookie | the user | `console.read`, plus `console.admin`/`console.user`/`console.pending` for the account level |
| Console API key | the key owner (must be an active admin) | `operator.api` |
| Sandbox token | the principal (`sub`), acting as the proxy's sandbox | the principal's sandbox flags, `slack.proxy`, the four `sandbox.*`, and `console.delegate` when the live admin link holds |
| Static admin key (`CENTAUR_APIRS_ADMIN_API_KEY`) | `api-rs-admin` | every api-rs capability (`Capability::API_RS`) |
| Ingress key | the bot | `sessions.read`/`sessions.write`, `workflows.events`, and for slackbot `workflows.actions`/`slack_external_teams.read`; platform-prefix scoped |
| API client token | the API client | its live scopes: `sessions.read` and `sessions.write` (+ `sessions.sandbox_release`) |
| MCP OAuth token | the MCP principal | unchanged; `/mcp` is its own protocol boundary and does not use this route catalog |

Operator (`operator.api`) is deliberately narrower than a Console session: user
approval, API clients, system settings, and workflow force start stay
`console.admin` routes, so a leaked API key cannot reach them. A bearer key on
a Console route therefore gets Console's `403 session_required`, not admin
access.

### Sandbox token

proxy-sync mints one token per assigned proxy and iron-proxy injects it once as
`Authorization: Bearer ...` with a host-only rule for every api-rs host,
including the Console host. There is no second token and no rule-order
dependency. Claims:

```json
{
  "iss": "centaur-console",
  "aud": "centaur-sandbox",
  "iat": 1700000000,
  "exp": 1700259200,
  "sub": "prn_...",
  "sandbox_id": "<proxy name>",
  "proxy_id": "prx_...",
  "capabilities": ["sessions.read", "workflows.read", "slack.proxy",
                   "sandbox.permissions", "sandbox.skills", "sandbox.tasks",
                   "sandbox.oauth_apps", "console.delegate"],
  "slack": {"upload_channels": [], "download_channels": [], "history_channels": []},
  "console_delegate": true
}
```

Signatures, `aud`, `iss`, and `exp` are checked on every request. The token is
never trusted alone: the guard re-reads the proxy -> principal binding (assigned,
same name, same principal) and, for `console.delegate`, re-checks that the
principal is still a Slack DM in the bot's home workspace linked to an active
Console admin. Reassigning a proxy or demoting/disabling the admin revokes
access on the next request. Sandbox secret writes keep their restriction: no
`control_plane`/`token_broker` sources, no editing OAuth-backed secrets, and no
secret deletes.

### Adding a route

```rust
let routes = GuardedRoutes::new(state)
    .console_admin("/principals", get(list).post(create)) // default level
    .console_user("/integrations", get(list))
    .console_pending("/me", get(me));
routes.into_router()
```

Use `console_admin` for admin-only endpoints, `console_user` for any active
account, `console_pending` only for endpoints pending accounts may reach,
`operator` for the bearer API, and `sandbox(path, route, capability)` for a
sandbox resource. api-rs's own routes (`routes.rs`) use `methods(path, route,
[(Method, Capability), ...])` when one path's methods differ, and
`merge_guarded(router, capability)` for a nested family.

The Console cookie guard keeps Console's browser rules in order: a disabled
account is rejected and its cookie reset; an unsafe method must carry an
`Origin` exactly equal to the Console public origin; an `Authorization` header
is never mixed with a cookie session. Pending accounts get `console.read` +
`console.pending`, so an admin route answers `403 account_pending` before any
Origin check (as before), and an active non-admin on an admin route answers
`403 admin_required`.

Error bodies are `{"error":{"message":...,"details":{"reason":...}}}`; use
`ConsoleError` in `http.rs` rather than building a new shape. The bearer and
sandbox surfaces keep their previous bodies (`invalid or missing API key` /
`invalid (or missing) sandbox token`); the unified guard picks the envelope from
the route's required capability, not from the URL.

## Session cookie and Origin

The cookie is named `_centaur_console_session`. It is AES-256-GCM encrypted
with a key derived from `CONSOLE_SESSION_SECRET`, carries a version, user id,
issue time, and optional `return_to`, and is capped at 30 days. It is
`HttpOnly; SameSite=Lax; Path=/`, adds `Secure` when the public origin is
`https://`, and intentionally has no `Max-Age` (a browser session cookie).
Sign-in rotates the cookie; logout and disabled accounts clear it. Decoding is
strict: the payload has no nested or unknown fields, so the never-shipped
variant that also carried a CSRF secret is rejected rather than ignored.

Unsafe methods are protected by an exact `Origin` check, not a CSRF token:

- Every non-GET/HEAD request on a cookie-authenticated route, including the
  console JSON API and every server-rendered form (`POST /login`,
  `POST /logout`, and `POST /mcp/oauth/authorize`), must send `Origin` exactly
  equal to the normalized `CENTAUR_CONSOLE_PUBLIC_URL` origin
  (`scheme://host[:port]`). The pending page's sign-out form posts to
  `POST /logout`; there is no `DELETE /logout`.
- A missing `Origin`, `Origin: null`, a different origin, or a trailing slash
  is refused. `Referer` is not a fallback.
- The JSON console API keeps its existing rejection: 403
  `{"error":{"message":"invalid or missing CSRF token","details":{"reason":
  "csrf"}}}`. Server-rendered forms answer with a 422 error page in the
  server-page layout.
- There is no CSRF token anywhere. Unknown form fields are ignored.
- GET and HEAD have no side effects and need no Origin. The cookie keeps
  `HttpOnly; SameSite=Lax`, and every response keeps `frame-ancestors 'none'`.

The SPA relies on the browser to send its own same-origin `Origin`; it does not
fetch or echo a token. A rejected write is a normal `ApiError`, with no
CSRF-refresh/retry.

The cookie carries no flash. A server-rendered error after a redirect is a
`GET /login?error=<code>` query parameter mapped by `pages::login_error` to the
same message text; `/api/v1/console/me` has no `flash` field, and the SPA shows
only the results of actions it performs itself.

`return_to` values must pass `return_path::safe`; never redirect to an
unvalidated path. It resolves the value against the Console origin and keeps the
path and query only when the origin is unchanged, so absolute URLs,
protocol-relative values, backslashes, whitespace, and control characters are
refused.

## Typed request parsing

Do not read `Json<Value>` and pick fields out of it. Deserialize each endpoint
into a narrow serde struct with one of the extractors in `request.rs`:

| Extractor | Wire shape |
| --- | --- |
| `ConsoleData<T>` | JSON `{"data":{...}}` (Console's document writes) |
| `ConsoleBody<T>` | a plain JSON body with no `data` wrapper |
| `ConsoleQuery<T>` | typed query parameters |
| `ConsoleForm<T>` | `application/x-www-form-urlencoded` form fields |

```rust
use serde::Deserialize;
use crate::console::request::ConsoleData;

#[derive(Deserialize)]
struct CreateRole {
    name: String,
    #[serde(default)]
    admin: Option<bool>,
}

async fn create_role(ConsoleData(data): ConsoleData<CreateRole>) -> Response {
    // data.name: String, data.admin: Option<bool>; unknown fields ignored.
    // ...
}
```

Rules shared by all four:

- Unknown fields are ignored. A nested form/query key such as `return_to[]` is
  simply an unknown field, so `return_to` is absent; there is no sentinel value.
- Field types are strict: a JSON string, number, or `null` is not a boolean, and
  there is no implicit casting. A wrong JSON field type (or a missing
  required field in a `ConsoleData`/`ConsoleBody` struct) is a 422
  `{"error":{"message":"validation failed","details":
  {"<field>":["<message>"]}}}`; the details key is the field path, so a handler
  can return the error unchanged. Declare intentionally optional fields as
  `Option<T>` or add `#[serde(default)]`.
- `ConsoleData` requires a non-empty `data` object. A missing `data` and any
  non-object `data` value (including `null`, `false`, `true`, numbers, strings,
  and arrays) are a 400 with the plain `missing required "data" object`.
  `ConsoleBody` treats an empty body as `{}`. The same 400 message is used in
  every namespace.
- A PATCH endpoint that must distinguish an absent field (keep the stored value)
  from an explicit `null` (clear it) keeps that distinction in the struct, for
  example with the existing `Patch` enum or with `Option<Option<T>>` plus
  `#[serde(default, deserialize_with = ...)]`; a plain `Option<T>` collapses
  both cases.
- `ConsoleQuery` and `ConsoleForm` deserialize the raw query/form string with
  `serde_urlencoded`; a parse failure is a Console 400 envelope. Bracket-shaped
  keys (`ids[]=`, `labels[team]=`) are ordinary unknown fields for a flat struct.
  `labels[key]=value` filters still use `Query<HashMap<String, String>>` with
  `ListParams`.

Pagination stays on `Pagination::from_query`/`ListParams`: `page`/`limit` are
plain `i64` integers (no `_` separators, prefix parsing, or unbounded
integers), with defaults (1/50) and clamps (page floored at 1, limit
1..=200). A non-integer keeps the existing 400 messages
(`page must be an integer` / `limit must be an integer`). Do not replace them
with a struct only to make a route "typed".

## Validation with garde

Field rules on a typed request are `garde` attributes, not hand-written checks.
`centaur_console_db::validation` turns garde's report into the
`FieldErrors` the 422 envelope already renders, so the wire shape does not
change: `{"error":{"message":"validation failed","details":{"<path>":["<msg>"]}}}`.
The admin area is the reference slice (`areas/admin/{users,api_clients,
system_settings,api_keys,proxies}.rs`).

```rust
use centaur_console_db::validation::{ValidateFields, rules};

#[derive(Deserialize, garde::Validate)]
struct UpdateNote {
    // An absent key leaves the stored title alone; an explicit null or a blank
    // value is the model's presence failure.
    #[serde(default, deserialize_with = "nullable_field")]
    #[garde(custom(rules::patch_not_blank))]
    title: Option<Option<String>>,
    // A reference the handler resolves; not a rule of the record.
    #[serde(default)]
    #[garde(skip)]
    owner: Option<String>,
}

async fn update(
    ...,
    ConsoleData(data): ConsoleData<UpdateNote>,
) -> Result<Response, ConsoleError> {
    data.validate_fields()?; // 422 with `details.title`, or nothing
    // ...
}
```

### The adapter

Everything lives in `centaur_console_db::validation`; a request struct only
needs `garde` (a workspace dependency) and the `ValidateFields` trait.

| Item | Purpose |
| --- | --- |
| `ValidateFields::validate_fields` | validate and get `Result<(), FieldErrors>`, `?`-ready |
| `ValidateFields::validate_fields_with` | the same, with the struct's `#[garde(context(..))]` context |
| `Validation` | collect garde reports *and* hand-written errors: `check`, `check_with`, `extend`, `add`, `add_base`, `finish_if_any`, `into_result` |
| `plain_message` | the first message of a report, for a gate the API answers with a plain 400 |
| `detail_key` | one garde path -> its `details` key |
| `rules::{present_not_blank, patch_not_blank, patch_included_in}` | the custom validators Console's models share, where garde has no equivalent rule |

`FieldErrors` stays the single field -> messages type, and
`impl From<FieldErrors> for ConsoleError` still renders the envelope, so a garde
failure, a data-layer validator failure (`centaur-console-db`'s secrets and
principals areas), and a hand-written check all return the same 422.

### Paths and keys

- A top-level field keeps its plain name. `#[garde(dive)]` nests with dots and
  `#[garde(inner(..))]` adds indices, which is exactly the path-keyed form the
  secrets document validators already use: `source.config`, `rules[0].cidr`.
  A rule on the record itself (a container-level `#[garde(custom(..))]`) reports
  under `base`.
- garde's derive validates a struct's named fields **alphabetically** (it keeps
  them in a `BTreeMap`) and reports one array per path. Report order and the
  message text inside an array are not contract; the set of `details` keys on a
  422 is. Nothing in the tree reorders a report to a model's
  declaration order.
- `#[garde(inner(..))]` reports per item in container order.
- `custom` receives the field as declared, so a create field (`Option<String>`)
  and a PATCH field (`Option<Option<String>>`) need different validators; that
  is why `rules` spells both `present_not_blank` and `patch_not_blank`.

### What stays hand-written

Anything that needs the database or the request context, because garde sees
neither:

- references that must resolve: an API client's `principal` (a 422 `base`), a
  proxy's `principal_id`/`requester_principal_id` (a 404), a settings write's
  `default_role_ids` (`"contains unknown roles: ..."`);
- uniqueness and ownership (`foreign_id` already taken, another user's row);
- cross-field rules that need the stored row.

Console's handlers check a reference *before* saving the record, so that
failure is reported alone: append it, `finish_if_any()?`, then `check` the
record. Do not merge a reference failure with field errors
(`areas/admin/api_clients.rs::create`). A pure query gate answers 400 with a plain message:
validate it with garde, then render `plain_message(&report)` through
`ConsoleError::bad_request` instead of the field envelope (`users::index`'s
`status` filter).

### Messages

Use garde's rules and its default messages. Validation message text, message
order, and messages-per-field count are not contract; a status, an
accept/reject decision, a route, and the `error.details` key set are.
Exceptions are one-liners a shared rule needs anyway (`"can't be blank"` via
`rules::{present_not_blank, patch_not_blank}`, `"is not included in the list"`
via `rules::patch_included_in`) and an area-specific rule where garde cannot
express the check (`"must include at least one scope"`), which keeps its own
string.

### What garde cannot express

- A rule about a collection as a whole that reports several messages, or that
  aggregates the offenders into one (`"contains unsupported values: a, b"`):
  a newtype with its own `garde::Validate` implementation, because a field rule
  returns one error (`Scopes` in `areas/admin/api_clients.rs`, `Labels` in
  `areas/admin/proxies.rs`). The same shape works for one message per map entry.
- A value that must be normalized (blanks dropped) before it is validated: do it
  in the newtype's `Deserialize` (`Scopes`), so the rule and the write see the
  same list.
- A protocol gate that answers neither the 422 envelope nor one plain message:
  an OAuth redirect carrying an error code, chosen by whichever check the
  handler reaches first (the MCP authorize endpoint's `response_type`, PKCE,
  scope, and resource checks). The code and the redirect target are not part of
  a garde report.
- An attribute name that differs from the parameter name: keep
  `FieldErrors::rename` (the skills handler reports `content` as
  `instructions`).
- Rules that need the database, the request context, or a stored row: the
  hand-written checks above.

## Shared HTTP conventions

- `ConsoleError::{bad_request,not_found,unprocessable,validation_failed}`
  produces Console's JSON envelope. `with_reason` and `with_details` add
  compatibility details; `validation_failed` is `"validation
  failed"` plus the field errors.
- `json_not_found` is the JSON 404 used by the Console-owned fallback
  (`/console`, `/api/v1`, `/mcp/oauth`, `/oauth`, `/assets/oauth-page`). Other
  unknown paths keep api-rs's empty 404.
- `DataPayload<T>` is Console's `{"data": ...}` envelope for a fixed-shape
  payload; `json_data` renders it as a 200, `created_data` as a 201, and
  `no_content` is the 204 every delete returns. Do not write the envelope as a
  `json!` map.
- `Pagination` reads `page`/`limit` (plain integer, defaults 1/50, limit clamped
  1..=200) and `ListParams` also reads `labels[key]=value` scalars. Its `meta`
  emits `page`, `limit`, `total`, `total_pages`; `total_pages` is 0 for no rows.
- `request_origin` is the request origin: the `Origin` header only, no
  `Referer` fallback. Every unsafe console request, JSON or form, requires it to
  equal `CENTAUR_CONSOLE_PUBLIC_URL`'s normalized origin; a missing or `null`
  Origin is rejected.
- Handlers set their own `Cache-Control` only where it matters: the static
  shell, hashed assets, `no-store` on `/me`, token responses, and sandbox
  permissions. There is no default cache layer and no `ETag`: nothing in the
  tree sends `If-None-Match`.

### Behavior deliberately left out

These behaviors were dropped. Do not reintroduce:

- SQL-ish 404 text (`Couldn't find X with [WHERE ...]`). A missing record is
  `not_found("<Model> not found")`.
- A framework 5xx page. The normal error envelope is used and the internal
  error is logged, never echoed.
- Format-suffix route aliases (`/up.json`, `/auth/:provider/callback.html`,
  `/mcp/oauth/token.json`). Register the canonical path only.
- A `/up` health page: api-rs serves `/healthz`.
- `/lookup/default/:foreign_id` compatibility aliases; only the canonical
  `/lookup/:foreign_id` lookup remains.
- Duplicate update verbs. Each resource keeps only the verb its callers send:
  console session routes take `PATCH` (the SPA) except the personal SSH key,
  which takes `PUT`; bearer provisioning takes `PUT` (iron-control, centaur-perms,
  e2e); sandbox admin routes take `PATCH` (connectors and tools).
- Server-side skill Markdown rendering (`content_html`): the API returns the raw
  `instructions` and the SPA renders it with sanitized React Markdown.
- Inspect-style quoting in messages. Quote values plainly.
- Silent type casts and dropped parameters: a field of the
  wrong JSON type is a 422 validation error, and an unknown field is ignored.
- MIME/Accept negotiation (`request_format`, wildcard/quality handling,
  format suffixes). A page route always answers HTML, a JSON endpoint always
  answers JSON, and the SPA shell is served for `/` and `/console/...` only.
- The SPA shell no longer writes `return_to` into the session; the SPA passes
  `?return_to=` to `/login` itself.
- Prefix integer parsing for `CENTAUR_MCP_ACCESS_TOKEN_TTL_SECONDS` and
  pagination. Both are strict now: the TTL must be a
  positive integer or the token endpoint returns its OAuth 400; `page`/`limit`
  must be plain `i64` values.
- Wide boolean casts for `CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED`
  and `CENTAUR_CONSOLE_API_CLIENTS_ENABLED`. Only `true`/`false`
  (case-insensitive) and `1`/`0` are accepted; an explicitly empty value keeps
  the third state, and anything else is a startup error.
- Id-sequence padding (writing child rows before a parent's validation).
  Validate first, then write everything in one transaction.

## Database

`AppState::console_session_db()` prefers the writable Console pool
(`with_console_write_db`) and falls back to the read-only turn-path pool.
Writes belong on the writable pool; the read-only pool's behavior is unchanged.

The workflow dashboard reads `centaur_readonly_workflow_runs` through
the runtime pool (`AppState::pool()`). Force
start calls the in-process `WorkflowRuntime` from `AppState::workflows()`; the
old `CENTAUR_API_URL` TCP probe is gone.
`centaur-console-db` changes should stay additive so the turn path keeps
compiling against the same functions.

Every `sync_config_cache_version` bump goes through
`centaur_console_db::invalidate::invalidate` (see "Abstractions" below). Its
`Scope::Grantee` covers grants: bump every principal the grant
reaches, directly or through its role. `Scope::BrokerCredential` bumps every
principal granted a secret whose `token_broker` source delivers that credential,
matching the principals a credential references;
call it at the same points that write commits. Reconciliation
inserts a grant with `ON CONFLICT DO NOTHING` on the
`(principal_id, static_secret_id)` index, so a race that already created the
grant is a no-op instead of an error. Ids are compared by insertion rank, so the
sequence value a conflicting insert consumes does not matter.

Console's Active Record encryption key is read in exactly one place,
`centaur_console_db::areas::encryption` (from
`CENTAUR_CONSOLE_AR_ENCRYPTION_PRIMARY_KEY` and
`CENTAUR_CONSOLE_AR_ENCRYPTION_KEY_DERIVATION_SALT`). Areas encrypt and decrypt
through it; do not read those variables again. `areas::encryption::keys` is the
only reader; the OAuth area's `OauthEncryption` builds its owned handle from
those keys (and keeps a key-injecting constructor for tests).

## Abstractions (use these)

The port was written area by area, so the same plumbing appears once per area.
These shared pieces are the canonical spelling; use them instead of a local
copy. The data-layer pieces live in
`crates/centaur-console-db/src/{model,write,invalidate,errors,timestamp}.rs`;
the HTTP pieces are in `console/http.rs`. Every area migrated so far
(`admin::api_keys` is the reference slice) shows all of them.

### Typed responses and rows

Deserialize requests with `request.rs` and render responses from typed structs;
do not assemble a `json!` map for a record that has a fixed shape. Database rows
derive `sqlx::FromRow` and are read with `query_as`:

```rust
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct ApiKeyRow {
    pub id: i64,
    pub name: String,
    pub created_at: time::PrimitiveDateTime,
    pub updated_at: time::PrimitiveDateTime,
}

#[derive(serde::Serialize)]
struct ApiKeyPayload {
    id: String,
    name: String,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    created_at: time::PrimitiveDateTime,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    updated_at: time::PrimitiveDateTime,
}

let row: ApiKeyRow = sqlx::query_as("SELECT id, name, created_at, updated_at FROM api_keys WHERE id = $1")
    .bind(id)
    .fetch_one(&pool)
    .await?;
```

### Timestamps

`centaur_console_db::timestamp` renders the Console `datetime` JSON: UTC RFC 3339
with exactly three fractional digits. Use the `serde` modules with
`#[serde(with = ...)]`:

| Field type | Module |
| --- | --- |
| `OffsetDateTime` | `centaur_console_db::timestamp` |
| `Option<OffsetDateTime>` | `centaur_console_db::timestamp::option` |
| `PrimitiveDateTime` | `centaur_console_db::timestamp::primitive` |
| `Option<PrimitiveDateTime>` | `centaur_console_db::timestamp::primitive_option` |

A hand-built payload uses the matching `json`/`json_option`/
`json_primitive`/`json_primitive_option` function. `timestamp::now()`,
`timestamp::datetime`, and `timestamp::offset` convert between Console's
`timestamp without time zone` columns and UTC. Do not format a timestamp by
hand; the old per-area helpers are now wrappers over this module.

### Model descriptor

`centaur_console_db::model::ConsoleModel` describes one table: `TABLE`,
`OID_PREFIX`, `NAME` (the model name a 404 renders), `HAS_FOREIGN_ID`,
and `SOFT_DELETE_COLUMN`. The markers in that module
(`model::ApiKey`, `model::Principal`, …) implement it for every Console table
the routes address. The generic lookups need the area's own `FromRow` row type:

```rust
use centaur_console_db::model::{self, ApiKey, ConsoleModel};

let key = model::find_by_oid::<ApiKey, ApiKeyRow, _>(&pool, oid)
    .await?
    .ok_or_else(ApiKey::not_found)?;       // 404 "ApiKey not found"
let by_foreign = model::find_by_ident::<ApiKey, ApiKeyRow, _>(&pool, ident).await?;
model::delete::<ApiKey, _>(&pool, key.id).await?; // soft delete via deleted_at
```

The descriptor and the row type are separate parameters: the marker names the
table, the area's `FromRow` struct is the projection it selects. `find_by_ident`
accepts an oid or a `foreign_id`, which is how the lookup routes address a row. A
model without an opaque id or without a soft-delete column says so in its
descriptor; do not re-derive the prefix or the column. A scoped lookup (another
user's key is not found) keeps its own `WHERE` and uses the marker only for
`not_found`/`oid`/`delete`.

### Timestamps on write

`centaur_console_db::write` builds the timestamped statements. Inserts always set
`created_at`/`updated_at`; updates bump `updated_at` **only when a column
changes**:

```rust
let sql = write::insert_returning_sql("api_keys", &["user_id", "name", "token_hash"], KEY_COLUMNS);
let row: ApiKeyRow = sqlx::query_as(&sql).bind(user_id).bind(name).bind(hash).fetch_one(pool).await?;

let sql = write::update_changed_sql("api_clients", &["name", "scopes"]);
sqlx::query(&sql).bind(name).bind(scopes).bind(id).execute(pool).await?;
```

`write::update_where_sql(table, columns, from_where)` is the same update
addressed by another column instead of the primary key, for a
`has_one dependent: :nullify` (`WHERE broker_credential_id = $2`).

Use plain SQL for a write that intentionally keeps `updated_at` (a soft delete
setting only `deleted_at`), a server-clock value on a
non-timestamp column, an `INSERT ... ON CONFLICT`, or a `COALESCE`/JSONB-merge
assignment.

### Invalidation

`centaur_console_db::invalidate::invalidate(conn, Scope)` is the only
sync-config invalidation. One `Scope` per affected row kind: `Principal`,
`Principals`, `Role`, `Grantee { principal_id, role_id }`,
`BrokerCredential(id)`, and `Secret { .. }` (built with
`Scope::secret_write(kind, id, SecretChanges)` or `Scope::secret_delete`). One
increment is counted per saved or destroyed record, so `SecretChanges` carries
the destroyed/inserted source and rule counts; do not flatten it to one bump per
principal.

```rust
use centaur_console_db::invalidate::{self, Scope, SecretChanges};

invalidate::invalidate(&mut tx, Scope::Principal(row.id)).await?;
invalidate::invalidate(
    &mut tx,
    Scope::secret_write(
        kind,
        id,
        SecretChanges::replaced(parent_changed, old_sources, new_sources, old_rules, new_rules),
    ),
)
.await?;
invalidate::invalidate(&mut tx, Scope::Grantee { principal_id, role_id }).await?;
```

Call it inside the same transaction, at the point the record is written.
Pass `&mut *tx` for a transaction.

### Validation errors

`centaur_console_db::FieldErrors` is the single field -> messages type (`base`
is an ordinary attribute), and
`impl From<FieldErrors> for ConsoleError` renders the 422 envelope, so a handler
can return it with `?`. Entries keep the order they were appended in; that
order is not contract.

A typed request declares its field rules as `garde` attributes and validates
them with `ValidateFields` (see "Validation with garde"):

```rust
use centaur_console_db::validation::{ValidateFields, rules};

#[derive(Deserialize, garde::Validate)]
struct CreateApiKey {
    #[serde(default)]
    #[garde(custom(rules::present_not_blank))]
    name: Option<String>,
}

data.validate_fields()?; // 422 `details.name` when the name is blank
```

The data-layer secret documents (`centaur-console-db::areas::secrets`) are
garde-validated typed document structs as well, reporting the nested
`source.config` / `rules[0].cidr` keys. A data-layer model that validates its own
fields does the same split the HTTP structs do: the field rules are `garde`
attributes on a rules struct built from the draft, and the rules needing the
database (uniqueness, a reference) stay hand-written — the principals area's
`areas/principals/rules.rs` is the example. Validators that still need the
decoded row or a nested record's attributes (a principal's grants) build
`FieldErrors` by hand. `Validation` combines their errors with a garde report
when one request has both.

```rust
let mut validation = Validation::new();
if !reference.is_empty() && principal.is_none() {
    validation.add_base(format!("No principal matches \"{reference}\"."));
}
validation.finish_if_any()?; // the reference wins
validation.check(&data);
validation.into_result()?;
```

`impl From<...> for ConsoleError` also exists for `centaur_console_db::Error`,
`areas::secrets::WriteError`, `areas::principals::PrincipalError`, and
`sqlx::Error`: a missing row is the 404, a validation failure is the 422, and
anything else is a logged opaque 500. Prefer `?` over an area-local
`write_error`/`failure` match.

`FieldErrors::rename` reports a validator's attribute under the parameter name
the client reads (the skills handler reports `content` as `instructions`).

### Lists

`http::ListParams` reads `page`/`limit` and `labels[key]=value`;
`http::labels_value` re-encodes those scalars for the `labels @> $n` containment
query. `http::list` runs the `COUNT` and the page query over the same filter and
renders `{data, meta}`:

```rust
const KEYS: ListSource = ListSource::new(
    "SELECT id, name, created_at, updated_at",
    "FROM api_keys WHERE user_id = $1 AND deleted_at IS NULL",
    "ORDER BY created_at ASC, id ASC",
    1, // filter parameters already bound in `from_where`
);

let params = ListParams::from_query(&query)?;
let body = list::<ApiKeyRow, ApiKeyPayload, _, _>(
    &pool,
    &params,
    KEYS,
    |arguments| bind(arguments, user_id),
    payload,
)
.await?;
```

The `bind` closure attaches the filter values (in `$1..=$n` order) and is called
once per statement; `bind` is the helper because sqlx's `PgArguments::add` is
private. `http::list_rows` runs the same two statements but returns the rows and
the total instead of rendering them, for a route whose payload needs state from
the whole page (the OAuth console index batches its apps' credential counts, and
a console grant list hydrates each row); pair it with
`http::list_response`.

## Pages, assets, and SSO

The built SPA is read from disk: `CENTAUR_CONSOLE_SPA_SHELL` names the shell
(default `services/console/public/spa/index.html`), `CENTAUR_CONSOLE_SPA_ASSETS`
the hashed asset root (default `services/console/public/spa/assets`), and
`CENTAUR_CONSOLE_PUBLIC_ROOT` the root public files (default
`services/console/public`). The api-rs image builds the SPA and sets all three
to `/app/services/console/...`; source maps are removed there.
`static_files.rs` serves them with `tower_http`'s `ServeDir`/`ServeFile`: the
shell for `/` and `/console/...` (`no-cache`, app CSP), `/spa/assets/*`
(`public, max-age=31536000, immutable`, gzip/brotli), and the root icons
(public-file cache). The shell is a plain file: a signed-out visitor is sent to
`/login?return_to=<page>` by the SPA when its first API call returns 401.
Other paths, including `/spa/index.html`, are not SPA routes and 404, so the
shell is only ever served with the CSP.

`/login` (GET/POST), `/pending`, and `POST /logout` are server-rendered.
Assets are embedded and served at `/assets/oauth-page/<name>?v=<digest>`. Form
bodies and the `?return_to=`/`?next=` query are parsed into the typed
`LoginForm`/`LoginQuery` structs in `pages.rs`; unknown fields are ignored. The
forms carry no token; their posts are subject to the same exact-Origin check as
the JSON API.
Every rendered response, redirect, and origin rejection from these pages carries
the narrowed server-page CSP (`script-src 'none'`,
`style-src 'self'`, no `form-action`); do not relax it. Other server-rendered
areas (the broker OAuth result pages) reuse the same `SERVER_PAGE_CSP` and
`server_page_csp` helpers from `pages.rs` instead of copying the policy.
`pages::server_error_page` is the shared HTML error template: the sign-in,
sign-out, and MCP consent forms' origin rejections use it with a 422.

HTML origin failures are 422, not the JSON API's 403 `csrf`. `require_login`
runs before the origin check, so `/logout` without a signed-in user (or with a
cookie for a deleted user) redirects to `/login` before checking the Origin.

SSO start/callback is routed at `/auth/{provider}/{start,callback}`. The start
page renders provider buttons from the same enabled-provider set the SSO routes
use. Do not add SSO routes through an unguarded router.

## Tests

Unit tests do not need a database. DB-backed tests use
`CONSOLE_DB_TEST_DATABASE_URL` (the same variable as
`centaur-console-db/tests`). They skip locally when it is unset and panic under
`CI`, so a CI misconfiguration cannot silently skip them. Migrate an empty
database first:

```bash
cargo run -p centaur-console-db --example migrate -- postgresql://...
cargo test --locked -p centaur-api-server console_session
```

The DB tests drive the real routers (`build_router_with_app_state`,
`console::bearer_router`, `sandbox_router`) and cover login -> `/me` -> Origin-checked write ->
logout, disabled/pending accounts, bearer owner states, the single sandbox
token and its live proxy binding, the delegated Console credential, and the
sandbox secret-write restriction. Unit tests in `host.rs`, `console::tests`,
`console::guard`, and `auth.rs` pin the public/internal path split, the
capability catalog and its implication hierarchy, and the credential ->
capability sets. CI runs them against a migrated Console database.

## iron-control calls

`IronControlClient` (session, warm-pool, workflow, and infra-role
registration) writes the Console database directly through the writable Console
pool, so api-rs requires `IRON_CONTROL_DATABASE_URL`. The calls resolve to the
same `centaur_console_db` functions the Console handlers use, so validation,
reconciliation, cache invalidation, and timestamps match the Console API.
Writes are attributed to the Console system user
(`centaur_console_db::SYSTEM_USER_EMAIL`), created on first use; it cannot sign
in and has no API keys. Only the external `centaur-perms` CLI calls the bearer
API.
