# Console Guide

## Role

The console is the Rust `centaur-console` binary that provides the
credential-control JSON API and serves the operator UI, a TypeScript React SPA
in `web/`. It manages principals, roles, grants, encrypted secret records,
proxies, broker credentials, console login, and MCP OAuth flows. Its code lives
in the workspace crates `centaur-console-db` (models, migrations),
`centaur-console-auth` (every credential scheme), `centaur-console-api` (the
axum routers, one module per area), and `centaur-console` (the binary); read
`crates/centaur-console-api/CONVENTIONS.md` before changing them. This
directory holds `web/`, the static files in `public/`, and the docs. The
binary and these files ship in the api-rs image (`services/api-rs/Dockerfile`).

The console replaced a Rails application
(`services/api-rs/rfcs/0006-console-api-in-rust.md`). Parity with Rails is not
a goal; the contract is Decision 5: paths, methods, status codes, JSON field
names and types, the error envelope, auth rules, side effects, the database
schema and rows, and JWT claims. Grep the SPA, `tools/infra/centaur-console`,
iron-control, the tools, and `e2e/tests` before changing a response.

Use `README.md` and `docs/API.md` for the supported behavior and API shapes.

## Architecture

- The server renders no UI except sign-in and the OAuth flows: no templates
  beyond the pages below, no browser JavaScript, and no asset pipeline. Every
  page after sign-in is a route in `web/`.
- The login and pending pages (`GET`/`POST /login`, `DELETE /logout`,
  `GET /pending`), the MCP consent page
  (`GET`/`POST /mcp/oauth/authorize`), and the broker OAuth result page
  (`/oauth/:slug/{start,callback}`) stay server-rendered so installed MCP
  clients and pods on either side of a rolling deploy see the protocol
  behaviour they expect, and sign-in works without JavaScript. They are
  standard HTML forms with an authenticity token, using the askama templates
  in `crates/centaur-console-api/templates` and plain CSS in
  `public/assets/oauth-page` (no
  JavaScript; keep its colours in step with `web/src/index.css`). Do not move
  them into the SPA, and keep their URLs, form field names, messages,
  validation order, and redirects stable. The SPA reaches `/login` and
  `/pending` with full page loads.
- The SPA fallback (`crates/centaur-console-api/src/assets.rs`) serves
  `spa-shell/index.html` for any browser navigation no route claims;
  `pnpm -C web build` puts only hashed assets in `public/spa`. Never put the
  shell back under `public/`: static files get no Content-Security-Policy. In
  the image they live under `/usr/share/centaur-console`
  (`CENTAUR_CONSOLE_PUBLIC_DIR`, `CENTAUR_CONSOLE_SPA_SHELL`). The other
  browser-facing routes
  (SSO `/auth/:provider/*`, broker OAuth `/oauth/:slug/*`, MCP OAuth
  `/mcp/oauth/*` and `/.well-known/*`, the PWA files, `/launch`) answer with
  redirects or JSON. Those namespaces, `/login`, `/logout`, and `/pending` are
  excluded from the SPA fallback, so unknown paths there get the JSON 404.
- Each console area is a module in `crates/centaur-console-api/src/`
  (`secrets`, `principals`, `identity`, `oauth`, `automation`, `misc`) that
  exposes `routes()`, plus `web/src/features/<area>/`. Handlers use plain
  `sqlx` queries (see `crates/centaur-console-api/CONVENTIONS.md`). Shared SPA
  code lives in `web/src/components/` (see its `README.md`), `web/src/lib/`,
  and the shell in `web/src/app/`. The SPA never relies on an authorization
  rule the API does not enforce.
- Post-login return paths go through `centaur_console_auth::return_path`;
  redirects never follow an unvalidated parameter.

## Invariants

- The namespace decides the credential, and the two never mix:
  - `/api/v1/*` is the bearer API (`Routes::bearer`, `BearerCaller`): a bearer API key
    whose owner is an active admin, on every action, exactly as for machine
    callers before the SPA. It does not accept the console session cookie.
    Keep its routes and responses stable; never add SPA
    fields, filters, or write semantics to it.
  - `/api/v1/console/*` is the console API (`Routes::console` and friends, `ConsoleCaller`): the console
    session only, never a bearer key (403 `session_required`, even alongside
    a cookie). Every endpoint the SPA calls lives here, and nothing else does.
    To serve a bearer resource to the SPA, register a console route that
    reuses the bearer handler's logic and put any console-only fields,
    filters, or write semantics there.
  - `/api/v1/sandbox/*` keeps its sandbox JWT schemes (including sandbox
    admin, which reuses bearer handlers under its own credential).
  - The namespace is enforced by the router: `app::router` mounts each
    `Routes` field under its prefix with its guard, so a handler cannot be
    mounted under the wrong credential, and a route in the wrong field fails
    closed with 401 `credential_namespace_mismatch`. `tests/base.rs` has the
    authorization matrix helpers (`assert_bearer_access`,
    `assert_console_access`).
  - API keys and proxies have no console endpoints, so a browser session
    cannot mint credentials that outlive it; admin actions such as user
    approval, API clients, settings, and workflow force start are console-only
    so a leaked API key cannot reach them.
  - Console writes require the session CSRF token (`X-CSRF-Token`) and a
    same-origin `Origin`.
  - Console endpoints are admin-only unless they are in the
    `console_active` field (scope records to their owner and 404 for
    non-owners) or `console_pending`; these exist only in the console
    API. Test new endpoints for the admin-only or active-user contract (both
    with the credential the path's namespace takes) plus ownership cases.
  - Keep proxy sync authentication, sandbox JWTs (including sandbox admin),
    and OAuth/MCP tokens as separate trust boundaries.
- In the SPA, reads use TanStack Query and a failed query renders an error
  alert, never an empty state. Writes are plain async functions in each
  feature's `api.ts` (request, then `invalidateQueries`), always to a
  `/api/v1/console/` path (`lib/api.ts` refuses anything else); ESLint bans
  `useMutation` so request bodies never enter a TanStack cache. Secret
  plaintext never enters a query key or cache: secret-bearing forms clear their
  secret fields after every submit, and the show-once API client secret stays
  in route-scoped component state. See `web/src/components/README.md`.
- Secret plaintext may be accepted for creation or rotation but must never be
  returned, logged, rendered, inspected in tests, or stored outside encrypted
  model attributes and configured providers.
- Apply authorization in handlers and the data layer before loading or
  mutating scoped resources. Test disabled users, non-admin users, namespace
  isolation, token replay, and ownership checks where relevant.
- OAuth/MCP changes must cover redirect validation, consent, PKCE, refresh-token
  family rotation and replay, revocation, account disablement, and identity
  reconciliation. A connected UI state alone is not proof of usable access.
- Keep handlers small, validate input in the handler, and preserve the JSON
  error envelope and pagination contracts.
- The schema is owned by SQLx migrations in
  `crates/centaur-console-db/migrations` (`0001_baseline.sql` is the Rails
  schema; never edit it). RFC 0006 freezes the schema for one release after
  the Rails removal so a rollback to the Rails image stays possible; add a
  migration only after that window, as the next numbered file.
- Use local fixtures or synthetic snapshots for UI work; do not make tests
  depend on a remote database.

## Content-Security-Policy

Every dynamic response carries an enforcing CSP; files under `public/` (SPA
assets, `public/assets/oauth-page`, icons, `offline.html`) are served before the
middleware and carry none, which is why the SPA fallback serves the shell.
`crates/centaur-console-api/src/pages.rs` and `app.rs` set the exact headers.

- Default (`pages::DEFAULT_CSP`): `default-src
  'self'; script-src 'self'; style-src 'self'; style-src-elem 'self'
  'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self';
  worker-src 'self'; manifest-src 'self'; object-src 'none'; base-uri 'none';
  frame-ancestors 'none'`.
- Server-rendered sign-in and OAuth pages (`pages::PAGE_CSP`, applied by
  `pages::render`): the same with `script-src 'none'`, plain
  `style-src 'self'`, and no `style-src-elem`.
- No inline or eval script. `web/index.html` must not contain an inline
  `<script>` or `on*=` handler; the pre-paint theme script is
  `web/theme-boot.js`, emitted hashed and injected into `<head>` by
  `vite.config.ts`. zod runs `jitless` (`main.tsx`) because its `new Function`
  probe trips `script-src`. Do not add `'unsafe-inline'`, `'unsafe-eval'`,
  hashes, or external origins to `script-src`.
- `style-src-elem 'unsafe-inline'` exists only because sonner and Radix's
  scroll lock inject `<style>` elements. `style=""` attributes stay blocked:
  set inline styles through React's `style` prop (CSSOM), never
  `setAttribute("style")` or raw HTML. Every fetch directive is `'self'`;
  self-host new images, fonts, or API origins or change the policy.
- Never set `form-action`, on any page. Browsers enforce it on the redirect
  after a form submission, and the MCP consent post
  (`POST /mcp/oauth/authorize`) redirects to the client's registered
  `redirect_uri` (`http://127.0.0.1:<port>`, `cursor://`, `vscode://`,
  `chrome-extension://`, `https://...`), as can the login post
  (`POST /login` -> `/mcp/oauth/authorize` -> error redirect).
  `form-action 'self'` would silently break MCP installs.
- After a policy, dependency, or `index.html` change, load the SPA (admin and
  member) and the full MCP flow (loopback and custom-scheme redirects, approve
  and deny) in a browser and fail on any `securitypolicyviolation` event or
  `Refused to` console message.

## Local database

Console runs on stock PostgreSQL 16 and needs no extensions beyond the
defaults, so managed services such as RDS and Cloud SQL work. For local
development, run PostgreSQL in Docker from `services/console`:

```bash
just postgres
```

This starts the pinned PostgreSQL image on `127.0.0.1:55432` and keeps its data
in a named Docker volume. `just dev` starts the same container automatically
and runs `centaur-console serve` against `iron_control_development`. When
using an existing PostgreSQL server, configure the `CENTAUR_CONSOLE_DB_*`
variables and set `CENTAUR_CONSOLE_MANAGE_POSTGRES=false`.

## Validation

Run cargo from the repository root. DB-backed tests need a disposable
PostgreSQL server whose user may create databases; each test clones its own
database from a migrated template:

```bash
export CONSOLE_DB_TEST_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:<port>/postgres

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p centaur-console-db -p centaur-console-auth -p centaur-console-api -p centaur-console
```

Without `CONSOLE_DB_TEST_DATABASE_URL` the DB-backed tests skip (and fail
under `CI`), so do not report DB coverage from a run without it. Endpoint
behaviour belongs in those Rust tests.

The SPA lives in `web/` (Vite, React, TypeScript, shadcn/ui, TanStack Query).
From `services/console`:

```bash
pnpm install --frozen-lockfile --filter @centaur/console-web
pnpm -C web typecheck
pnpm -C web lint
pnpm -C web test
pnpm -C web build   # assets into public/spa, shell into spa-shell/ (served by centaur-console)
```

SPA tests cover logic and secret handling only (API client, formatting, local
time, pagination clamping, the Slack channel picker label, the secret form
model and field, the shell and navigation).
`web/src/features/secret-handling.test.tsx` checks that secret plaintext
leaves the inputs and TanStack caches after every secret-bearing submit and
that an issued API client secret is shown once. Check page behaviour in a
browser.

`just ci` runs the SPA checks and build, then the console crates' format,
clippy, and tests. Add focused Rust tests with each behavior change. For
visible UI changes, run `just dev`, exercise the affected flow in a browser,
and check narrow and wide layouts plus keyboard/focus states. For a chart or
image change, build the api-rs image (`just build-one console` from the
repository root) and deploy it to the local stack.
