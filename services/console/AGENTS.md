# Console Guide

## Role

The console is a Rails application that provides the credential-control JSON
API and serves the operator UI, a TypeScript React SPA in `web/`. It manages
principals, roles, grants, encrypted secret records, proxies, broker
credentials, console login, and MCP OAuth flows.

Use `README.md` and `docs/API.md` for the supported behavior and API shapes.

## Architecture

- Rails renders no UI except sign-in and the OAuth flows: no views (beyond the
  unused mailer layouts and the pages below), view helpers, browser JavaScript,
  or asset pipeline. Every page after sign-in is a route in `web/`.
- The login and pending pages (`GET`/`POST /login`, `DELETE /logout`,
  `GET /pending`; `SessionsController`), the MCP consent page
  (`GET`/`POST /mcp/oauth/authorize`), and the broker OAuth result page
  (`/oauth/:slug/{start,callback}`) stay server-rendered so installed MCP
  clients and pods on either side of a rolling deploy see the protocol
  behaviour they expect, and sign-in works without JavaScript. They are
  standard Rails forms with an authenticity token, using
  `layouts/oauth.html.erb` and plain CSS in `public/assets/oauth-page` (no
  JavaScript; keep its colours in step with `web/src/index.css`). Do not move
  them into the SPA, and keep their URLs, form field names, messages,
  validation order, and redirects stable. The SPA reaches `/login` and
  `/pending` with full page loads.
- `SpaController` (`lib/spa_request.rb`) serves `spa-shell/index.html` for any
  browser navigation no route claims; `pnpm -C web build` puts only hashed
  assets in `public/spa`. Never put the shell back under `public/`: static
  files get no Content-Security-Policy. The other browser-facing Rails routes
  (SSO `/auth/:provider/*`, broker OAuth `/oauth/:slug/*`, MCP OAuth
  `/mcp/oauth/*` and `/.well-known/*`, the PWA files, `/launch`) answer with
  redirects or JSON. Those namespaces, `/login`, `/logout`, and `/pending` are
  excluded from the SPA fallback, so unknown paths there get the JSON 404.
- Each console area owns `config/routes/console_<area>.rb` (its
  `/api/v1/console` routes and protocol routes) and `web/src/features/<area>/`;
  the shell's endpoints are in `config/routes/console.rb`, the bearer and
  sandbox API in `config/routes/api.rb`. Shared SPA
  code lives in `web/src/components/` (see its `README.md`), `web/src/lib/`,
  and the shell in `web/src/app/`. Business logic stays in models and
  `app/services`; the SPA never relies on an authorization rule the API does
  not enforce.
- Post-login return paths go through `ConsoleReturnPath`; redirects never
  follow an unvalidated parameter.

## Invariants

- The namespace decides the credential, and the two never mix:
  - `/api/v1/*` is the bearer API (`Api::BaseController`): a bearer API key
    whose owner is an active admin, on every action, exactly as for machine
    callers before the SPA. It does not accept the console session cookie.
    Keep its routes and responses stable; never add SPA
    fields, filters, or write semantics to it.
  - `/api/v1/console/*` is the console API (`Api::V1::Console`,
    `ConsoleApiAuthentication`): the console session only, never a bearer key
    (403 `session_required`, even alongside a cookie). Every endpoint the SPA
    calls lives here, and nothing else does. New SPA endpoints go in
    `Api::V1::Console::BaseController` subclasses; to serve a bearer resource
    to the SPA, add a thin subclass of the bearer controller that includes
    `ConsoleApiAuthentication` (like the sandbox admin API) and put any
    console-only fields, filters, or write semantics in overrides there.
  - `/api/v1/sandbox/*` keeps its sandbox JWT schemes (including sandbox
    admin, which subclasses bearer controllers and skips their auth).
  - A console controller reached outside `/api/v1/console`, or a bearer
    controller routed under it, fails closed (raises in development and test,
    401 in production). `test/controllers/api/v1/credential_policy_test.rb`
    sweeps every route.
  - API keys and proxies have no console endpoints, so a browser session
    cannot mint credentials that outlive it; admin actions such as user
    approval, API clients, settings, and workflow force start are console-only
    so a leaked API key cannot reach them.
  - Console writes require the session CSRF token (`X-CSRF-Token`) and a
    same-origin `Origin`.
  - Console endpoints are admin-only unless they opt in with
    `allow_active_users` (scope records with `owned_scope` /
    `require_owner_or_admin!`, which 404s for non-owners) or
    `allow_pending_users`; these exist only in the console API. Test new
    endpoints with `assert_admin_only_api` or `assert_active_user_api` (both
    check the credential the path's namespace takes) plus ownership cases.
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
- Apply authorization in controllers and service objects before loading or
  mutating scoped resources. Test disabled users, non-admin users, namespace
  isolation, token replay, and ownership checks where relevant.
- OAuth/MCP changes must cover redirect validation, consent, PKCE, refresh-token
  family rotation and replay, revocation, account disablement, and identity
  reconciliation. A connected UI state alone is not proof of usable access.
- Put business logic in models or `app/services`, keep controllers thin, and
  preserve JSON error and pagination contracts.
- Generate migrations with Rails and commit the resulting `db/schema.rb` change.
  Do not hand-edit the schema dump.
- Use local fixtures or synthetic snapshots for UI work; do not make tests
  depend on a remote database.

## Content-Security-Policy

Every Rails response carries an enforcing CSP; files under `public/` (SPA
assets, `public/assets/oauth-page`, icons, `offline.html`) are served before the
middleware and carry none, which is why `SpaController` serves the shell.
`test/integration/content_security_policy_test.rb` pins the exact headers.

- Default (`config/initializers/content_security_policy.rb`): `default-src
  'self'; script-src 'self'; style-src 'self'; style-src-elem 'self'
  'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self';
  worker-src 'self'; manifest-src 'self'; object-src 'none'; base-uri 'none';
  frame-ancestors 'none'`.
- Server-rendered sign-in and OAuth pages (`ServerRenderedPagePolicy`, applied
  before every other callback): the same with `script-src 'none'`, plain
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
in a named Docker volume. `just dev` starts the same container automatically.
When using an existing PostgreSQL server, configure the `CENTAUR_CONSOLE_DB_*`
variables and set `CENTAUR_CONSOLE_MANAGE_POSTGRES=false`.

## Validation

From `services/console`, point Rails at the Docker PostgreSQL instance before
running database tasks or tests:

```bash
export CENTAUR_CONSOLE_DB_HOST=127.0.0.1
export CENTAUR_CONSOLE_DB_PORT=55432
export CENTAUR_CONSOLE_DB_USERNAME=postgres
export CENTAUR_CONSOLE_DB_PASSWORD=postgres

just postgres
bundle install
bin/rails db:prepare
bin/rails test
bin/rubocop
bin/brakeman --quiet --no-pager --exit-on-warn --exit-on-error
```

The SPA lives in `web/` (Vite, React, TypeScript, shadcn/ui, TanStack Query).
From `services/console`:

```bash
pnpm install --frozen-lockfile --filter @centaur/console-web
pnpm -C web typecheck
pnpm -C web lint
pnpm -C web test
pnpm -C web build   # assets into public/spa, shell into spa-shell/ (served by SpaController)
```

SPA tests cover logic and secret handling only (API client, formatting, local
time, pagination clamping, the Slack channel picker label, the secret form
model and field, the OAuth app form payload, the shell and navigation).
`web/src/features/secret-handling.test.tsx` checks that secret plaintext
leaves the inputs and TanStack caches after every secret-bearing submit and
that an issued API client secret is shown once. Endpoint behaviour belongs in
Rails request tests; check page behaviour in a browser.

`bin/ci` (`just ci`) runs RuboCop, the gem audit, Brakeman, the SPA checks and
build, Rails tests, and seed validation. When several checkouts share one
PostgreSQL server, give each its own `CENTAUR_CONSOLE_TEST_DB_NAME` and
`CENTAUR_CONSOLE_DEV_DB_NAME`, and cap `PARALLEL_WORKERS`.
Add focused controller/model/service tests with each
behavior change. For visible UI changes, run `just dev`, exercise the affected
flow in a browser, and check narrow and wide layouts plus keyboard/focus states.
