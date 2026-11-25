# Console Guide

## Role

The Console is the credential-control surface: the JSON API, the operator UI,
principals, roles, grants, encrypted secret records, proxies, broker
credentials, console login, and MCP OAuth flows. api-rs serves all of it:

- `crates/centaur-api-server/src/console/`: routes, guard, sessions,
  server-rendered pages, and static files. Read its `README.md` before changing
  Console behavior.
- `crates/centaur-console-db/`: queries, writes, validation, encryption, and
  the Console schema migrations.

This directory holds only the operator UI (`web/`, a TypeScript React SPA), the
static files api-rs serves or embeds (`public/`), and the API reference
(`docs/API.md`). Use `README.md` and `docs/API.md` for the supported behavior
and API shapes, and keep `docs/API.md` in step with API changes.

## Architecture

- The server renders no UI except sign-in and the OAuth flows. Every page after
  sign-in is a route in `web/`.
- The login and pending pages (`GET`/`POST /login`, `POST /logout`,
  `GET /pending`), the MCP consent page (`GET`/`POST /mcp/oauth/authorize`),
  and the broker OAuth result page (`/oauth/:slug/{start,callback}`) stay
  server-rendered so installed MCP clients see the protocol behaviour they
  expect, and sign-in works without JavaScript. They use the plain CSS in
  `public/assets/oauth-page` (no JavaScript; keep its colours in step with
  `web/src/index.css`). Do not move them into the SPA, and keep their URLs,
  form field names, messages, validation order, and redirects stable. The SPA
  reaches `/login` and `/pending` with full page loads.
- `pnpm -C web build` writes the shell to `public/spa/index.html` and hashed
  assets to `public/spa/assets`. api-rs serves the shell, with the
  Content-Security-Policy, only for `/` and `/console/...`; `/spa/assets`
  serves the assets directory alone. Do not add a route that serves the shell
  or other HTML without the policy.
- Each console area owns `web/src/features/<area>/`. Shared SPA code lives in
  `web/src/components/` (see its `README.md`), `web/src/lib/`, and the shell in
  `web/src/app/`. The SPA never relies on an authorization rule the API does
  not enforce.
- Post-login return paths are validated server-side
  (`console/return_path.rs`); redirects never follow an unvalidated parameter.

## Invariants

- api-rs has one authorization model. Every credential maps to an
  `AuthenticatedCaller` with a set of capabilities, and every route declares
  the capability it requires (`console::guard::GuardedRoutes`, shared with
  api-rs's own routes).
  - `/api/v1/console/*` provides the console API. A session cookie
    authenticates the browser (`console.read` plus the account's
    `console.admin`/`console.user`/`console.pending` level), and a delegated
    sandbox credential acts as its linked active admin (`console.delegate`).
    An `Authorization` header is never mixed with a cookie session. Every
    endpoint the SPA calls lives here, and nothing else does.
  - `/api/v1/*` outside the console and sandbox namespaces is the bearer API:
    a Console API key whose owner is an active admin, holding `operator.api`
    and nothing else. User approval, API clients, system settings, and workflow
    force start are `console.admin` routes, so an API key cannot reach them.
  - `/api/v1/sandbox/*` uses the single sandbox token from proxy-sync.
  - The route's required capability, not the path, selects the accepted
    credential. A credential without that capability fails closed. There is no
    unguarded way to add an authenticated route.
  - Keep proxy sync authentication, the sandbox token, and OAuth/MCP tokens as
    separate trust boundaries. OAuth/MCP keeps its existing protocol
    authentication and does not use the route capability catalog.
- Console writes require an exact same-origin `Origin` for cookie sessions.
  There is no CSRF token. A delegated sandbox caller has no cookie and is
  exempt from the Origin check.
- Console endpoints are admin-only unless they declare `console.user` (skills,
  scheduled tasks, personal SSH key, integrations, scoped to the owner) or
  `console.pending` (`/me`, `/session`).
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
  columns and configured providers.
- OAuth/MCP changes must cover redirect validation, consent, PKCE, refresh-token
  family rotation and replay, revocation, account disablement, and identity
  reconciliation. A connected UI state alone is not proof of usable access.
- Add schema changes as SQLx migrations in
  `crates/centaur-console-db/migrations`.
- Use local fixtures or synthetic snapshots for UI work; do not make tests
  depend on a remote database.

## Content-Security-Policy

api-rs sets an enforcing CSP on the SPA shell (`static_files::APP_CSP`) and on
the server-rendered pages (`pages.rs`); its tests pin the exact headers.

- App policy: `default-src 'self'; script-src 'self'; style-src 'self';
  style-src-elem 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self';
  connect-src 'self'; worker-src 'self'; manifest-src 'self'; object-src
  'none'; base-uri 'none'; frame-ancestors 'none'`.
- Server-rendered sign-in and OAuth pages: the same with `script-src 'none'`,
  plain `style-src 'self'`, and no `style-src-elem`.
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
- The Vite dev server sends no CSP. After a policy, dependency, or
  `index.html` change, load the SPA as api-rs serves it (admin and member) and
  the full MCP flow (loopback and custom-scheme redirects, approve and deny) in
  a browser and fail on any `securitypolicyviolation` event or `Refused to`
  console message.

## Validation

From the repository root:

```bash
pnpm install --frozen-lockfile --filter @centaur/console-web
pnpm -C services/console/web typecheck
pnpm -C services/console/web lint
pnpm -C services/console/web test
pnpm -C services/console/web build
```

SPA tests cover logic and secret handling only (API client, formatting, local
time, pagination clamping, the Slack channel picker label, the secret form
model and field, the OAuth app form payload, the shell and navigation).
`web/src/features/secret-handling.test.tsx` checks that secret plaintext
leaves the inputs and TanStack caches after every secret-bearing submit and
that an issued API client secret is shown once. Endpoint behaviour belongs in
the api-rs Console tests; see `crates/centaur-api-server/src/console/README.md`.

For visible UI changes, run the dev server against a local Console (see
`README.md`), exercise the affected flow in a browser, and check narrow and
wide layouts plus keyboard/focus states.
