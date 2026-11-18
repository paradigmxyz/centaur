# RFC 0006: Console API in Rust

Status: accepted

## Goal

Replace the Rails console (`services/console`, ~15k lines of app + lib,
~27k lines of tests) with Rust in the repository workspace, with **no
observable behaviour change** for any caller: the SPA, the bearer API, the
sandbox API, MCP clients, proxy-sync, api-rs, tools, and the e2e suite. The
SPA in `services/console/web` is kept as-is and served by the Rust binary.

## Surface to port

| Area | Routes / pieces | Notes |
| --- | --- | --- |
| Bearer API `/api/v1/*` | 8 secret kinds (CRUD + lookup), roles, principals (+effective_config, slack perms, roles, grants), grants, api_keys, proxies, scheduled_tasks#show, broker_credentials, oauth_apps | Called by iron-control, centaur-console CLI, e2e. Byte-stable. |
| Sandbox API `/api/v1/sandbox/*` | permissions, oauth_apps, scheduled_tasks (+run), skills (+share/editors), admin/* (principals, grants, secrets, 3 secret kinds, slack external teams, roles) | Sandbox entitlement JWT; sandbox-admin scheme. |
| Console API `/api/v1/console/*` | me, session, grants, slack_channels, principals, roles, secrets (+role grants, 4 kinds), broker_credentials, oauth_apps (+presets, providers, integrations), users, api_clients, system_settings, workflows, scheduled_tasks, skills, me/ssh_key | Session cookie + `X-CSRF-Token` + same-origin `Origin`; admin-only by default. |
| Server-rendered | `/login`, `/logout`, `/pending`, `/auth/:provider/{start,callback}` (Google, Slack OIDC, Okta), `/mcp/oauth/*`, `/.well-known/*`, `/oauth/:slug/{start,callback}`, `/manifest`, `/service-worker`, `/launch`, `/up` | Field names, messages, validation order, redirects, CSP pinned. |
| SPA | shell with CSP, hashed assets in `public/spa`, fallback rules from `SpaRequest`, JSON 404 | |
| Jobs (Solid Queue) | scheduled-task scheduler (1m) + run, slack channel catalog (1h) + membership (10m), Slack DM sync (10m), Google Docs sync (30m + per-doc fetch queue), Granola sync (30m), OAuth identity enrichment, finished-job cleanup | See Decision 3. Rails.cache used only for a refresh lock and OIDC discovery cache. |
| Boot | `db:prepare`, `Iron::Bootstrap` (initial user / API key), seeds | |

Already in Rust: `centaur-console-db` (read side used by api-rs, proxy-sync,
iron-control), `active-record-encryption` (AES-GCM, compression on read),
`centaur-token-broker` (broker refresh), oid/sqids, MCP JWT verification in
api-rs.

## Decision 1: where it lives

Options:

1. **Inside the api-rs process** (routes merged into `centaur-api-server`).
   Removes the console -> api-rs HTTP hop. But api-rs would gain the
   ActiveRecord encryption keys, console DB write credentials,
   `SECRET_KEY_BASE`, IdP client secrets, and a browser-facing ingress; today
   it has none of these and is the service sandboxes and ingresses talk to.
   Restarts and rollouts also couple.
2. **Separate service and image.** Clean isolation, but a parallel stack of
   startup, telemetry, and config code.
3. **In the api-rs workspace and image, separate Deployment (recommended).**
   New crates under `crates/`, shared with api-rs (`centaur-console-db`,
   `centaur-telemetry`, JWT, oid, encryption). The api-rs image gains a
   `centaur-console` binary (or `centaur-api-server console` subcommand) that
   the chart runs as the existing `console` and `console-worker`
   Deployments. The Service name, port 3000, network policies, secrets, and
   public URL stay the same, so iron-proxy rules, sandbox paths, MCP issuer
   metadata, tools, and `IRON_CONTROL_URL` need no change. Collapsing into
   the api-rs process later is a chart change, not a rewrite.

Crate layout:

- `centaur-console-db` - grows from read-only queries into the full model
  layer (writes, validations, callbacks such as sync-config invalidation)
  plus the migrations. Existing readers keep their API.
- `centaur-console-auth` - session cookie, CSRF, API key, sandbox entitlement
  JWT, sandbox admin, MCP JWT issue, login providers.
- `centaur-console-api` - axum routers per area (mirrors
  `config/routes/console_<area>.rb`), JSON/error/pagination conventions, the
  credential-namespace policy, CSP, SPA serving, server-rendered pages
  (askama templates using the existing `public/assets/oauth-page` CSS).
- `centaur-console-jobs` - background jobs and their runner.
- `centaur-console` (bin) - config (`ConsoleEnv` naming with `IRON_CONTROL_*`
  / `CENTAUR_CONSOLE_*` aliases), startup, `migrate`, `bootstrap`, `serve`,
  `worker` subcommands.

## Decision 2: database and migrations

- **Keep the database and schema unchanged.** Same `iron_control_production`
  DB, same tables, columns, indexes, ids, oids, encrypted payload format,
  bcrypt password digests, and SHA-256 token hashes. Rust readers and Rails
  can run against the same rows during the transition and rollback.
- **Schema ownership moves to SQLx.** `crates/centaur-console-db/migrations/`
  starts with `0001_baseline.sql`, a `pg_dump --schema-only` of a database
  migrated to Rails version `20261109021725`. On startup `migrate` does:
  - fresh DB (no `schema_migrations`): run all SQLx migrations;
  - Rails-managed DB whose `schema_migrations` max version is exactly
    `20261109021725`: record the baseline as applied in `_sqlx_migrations`
    without running it, then apply later migrations;
  - any other Rails version: refuse to start (the Rails image must finish
    migrating first).
  A CI test migrates a DB with Rails, adopts it with Rust, and diffs
  `pg_dump --schema-only` against a fresh Rust install.
- **Migration freeze.** No new Rails migrations once this starts. No new SQLx
  migrations until one release after cutover, so rolling back to the Rails
  image stays possible. Rails' `schema_migrations` / `ar_internal_metadata`
  stay in place (unused) until the Rails code is deleted.
- **Solid Queue / Cache / Cable databases are retired** after cutover, with
  no replacement queue in the console (see Decision 3). The refresh lock
  becomes a Postgres advisory lock; the OIDC discovery cache becomes
  in-process with TTL. At cutover the Rails worker scales to 0 and queued
  Solid Queue jobs are dropped: catalog jobs are recreated by their loops,
  scheduled-task runs not yet executed at cutover are dropped, and v1 sync
  jobs are deprecated. The `*_cache`, `*_queue`, `*_cable` DBs are dropped by
  a later chart release, never automatically.
- The read-only connection to the api-rs DB (`CENTAUR_DATABASE_URL`,
  `centaur_readonly_*` views for workflows) stays.

## Decision 3: background work

| Rails job | Outcome |
| --- | --- |
| `ScheduledTaskSchedulerJob` + `ScheduledTaskRunJob` | Moves to api-rs on Absurd (below). |
| `SlackChannelCatalogRefreshJob` (1h) | `console worker` loop under a Postgres advisory lock; also triggered by `enqueue_if_empty`. |
| `SlackChannelCatalogMembershipRefreshJob` (10m, and per channel on permission change) | Same loop; the per-channel refresh is a best-effort spawned task covered by the periodic pass. |
| `Oauth::EnrichCredentialIdentityJob` | Best-effort spawned task from the OAuth callback (it already swallows failures). |
| `SlackDm::*`, `GoogleDocs::*`, `Granola::*` (company context v1) | **Deprecated, not ported.** `centaur-company-context` covers Drive, Granola, and Slack including DMs. |
| `GoogleDocs::SyncCredentialJob`, `SlackDm::PollSyncJob`, finished-job cleanup | Compatibility shims and Solid Queue housekeeping; dropped. |

`console worker` is therefore a single replica-safe process with two
advisory-locked loops and no queue.

### Scheduled tasks on Absurd in api-rs

The `scheduled_tasks` table, its CRUD endpoints, validations, and
`next_run_at` display stay in the console. api-rs owns *when* and *whether* a
run happens, reusing the workflow schedule-tick pattern in
`centaur-workflows` (`run_schedule_tick`: durable `sleep_until`, idempotent
spawn, then chain the next tick). api-rs keeps read-only console DB access;
every console write goes through the console.

- **Reconcile.** A `scheduled_task.tick` Absurd task per enabled task with a
  cron. api-rs reconciles from the console DB (read-only) on an interval and
  when the console notifies it after create, update, enable, disable, or
  delete (`POST /api/admin/scheduled-tasks/{oid}/reconcile`, console service
  JWT). The tick's idempotency key is
  `scheduled-task-tick:{id}:{scheduled_for}`, so reconcile and chaining
  never create two ticks for one occurrence.
- **Fire.** On wake the tick runs Absurd steps:
  1. `claim`: `POST {console}/internal/v1/scheduled_tasks/{oid}/claim
     {scheduled_for}`. In one locked transaction the console checks enabled
     and `next_run_at == scheduled_for` (or already claimed for it, for
     replay), advances `next_run_at` and `last_enqueued_at` exactly as
     `ScheduledTaskSchedulerJob` does, provisions the execution principal
     (`ConsoleUserPrincipalProvisioner`), checks the delivery policy, and
     returns `api_input` or a skip/error reason (recording `last_error`).
     Provisioning and delivery policy stay in the console because they write
     console rows.
  2. `spawn`: spawn the `console_workflow` run on the standard queue with
     the existing idempotency key
     `scheduled-task:{numeric id}:{scheduled_for UTC iso8601}` and
     `max_attempts: 3`. Keeping the key format means a run Rails created
     just before cutover is not duplicated.
  3. `report`: `POST .../result {run_id | error}` sets `last_run_id`,
     `last_run_at`, `last_error` as `ScheduledTaskRunJob` does.
  4. Chain the next tick from the console's new `next_run_at`.
- **Run now** (console and sandbox `POST .../run`): the console calls api-rs
  `POST /api/admin/scheduled-tasks/{oid}/run` to spawn a manual tick with
  `scheduled_for = now`; claim skips the `next_run_at` check for manual
  runs. The 202 `queued: true`, 422 `task_disabled`, and 503 responses keep
  their shapes.
- **Cron semantics.** Standard 5-field Unix cron (0 and 7 are Sunday) with
  an IANA `timezone`, evaluated by the `croner` crate through one small
  shared helper used by the console (validation, `next_run_at`) and api-rs.
  fugit is not reproduced; results only need to agree with it on the
  console's presets and simple weekly schedules.
- **Cutover.** Ships with the Rust console. On
  first reconcile, api-rs creates ticks from the stored `next_run_at`, so
  occurrences due during the switch fire once (claim is a compare-and-set;
  the spawn key matches Rails'). A run Rails' scheduler had already claimed but whose
  `ScheduledTaskRunJob` was still queued when Solid Queue was dropped is
  lost: Rails-queued runs not yet executed at cutover are dropped. Rolling back to the previous release restores the Rails
  scheduler and removes the api-rs runner; the same keys prevent double runs.
  RFC 0006a is the wire contract.

### Deprecating company context v1

- The Rust console does not port the v1 jobs or their services
  (`SlackDm::SyncCredential`, `GoogleDocs::SyncCredential`,
  `Granola::SyncCredential`, `SlackApi` paths used only by them).
- `companyContextV1.enabled` and `console.googleDocsSync` are removed from the chart
  (with a NOTES/deprecation entry); the chart's `console.slackDmSync.
  oauthAppSlug` stays because `centaur-company-context` reads it.
- Data already indexed through the v1 api-rs ingest endpoints stays
  searchable. Removing `/api/admin/{slack/dm-sync,google/docs-sync,
  granola/sync}/*` and their tables is a separate api-rs change after
  `experimentalCompanyContext` is the supported path.
- `slack_dm_sync_cursors` stays in the schema (unused) until a later
  migration after the rollback window.

## Decision 4: sessions and CSRF (revised)

The console uses its own session and CSRF design; it does not reproduce
Rails' cookie, `MessageEncryptor`, or masked-token formats. Cutover and any
rollback sign everyone out once.

- Session: an encrypted, authenticated cookie (AES-256-GCM via the `cookie`
  crate's private jar) holding the user id, issue time, and a CSRF secret,
  keyed from a key derived from `SECRET_KEY_BASE`. HttpOnly, SameSite=Lax,
  Secure when the request or public URL is HTTPS, absolute expiry. The
  session is renewed on sign-in (no fixation) and cleared on sign-out.
- CSRF: the session's random token is returned as `csrf_token` from
  `/api/v1/console/me` and required in `X-CSRF-Token` (SPA writes) or the
  `authenticity_token` field (server-rendered forms), compared in constant
  time, plus a same-origin `Origin` check on every console write and form
  post.
- OAuth/SSO `state` values are HMAC-signed with a derived key and carry an
  expiry.

## Decision 5: compatibility contract (revised)

There is no byte-for-byte parity requirement. The Rust console keeps the
contracts other software depends on, and is otherwise free to use ordinary
Rust/axum/sqlx conventions:

- Database: same schema and existing rows (ActiveRecord-encrypted columns
  are read and written in the existing format via `active-record-encryption`,
  bcrypt digests, SHA-256 token hashes, sqids oids).
- External callers (bearer API, sandbox API, MCP OAuth, iron-control, the
  `centaur-console` CLI, tools, the SPA): same paths and methods, status
  codes, JSON field names and types, and the error envelope
  `{"error":{"message":...,"details":...}}`. Field order, timestamp
  precision, exact validation message texts, and Rails-specific HTML error
  pages are not part of the contract; the SPA is updated where it relied on
  one.
- JWT claims and secrets verified by api-rs, iron-proxy, and proxy-sync.
- Server-rendered sign-in, MCP consent, and broker OAuth pages: same URLs,
  form field names, and redirects.

Ruby and Rails behaviour that only existed as framework detail (Rack
conditional GET and ranges, Ruby integer/URI/IPAddr/YAML semantics, fugit,
ActiveModel error ordering, Rails routing format suffixes, browser
allow-listing) is not reproduced; standard crates are used instead.

## Cutover and rollback

Rails is deleted in the same change; there is no implementation flag.

1. The change replaces `services/console/{app,lib,config,db,test,bin,
   Gemfile*,Rakefile,config.ru,...}` (keeping `web/` and
   `public/assets/oauth-page`), Ruby CI, the Rails image, and the Solid DB
   init and wiring. The `console` and `console-worker` Deployments run the
   Rust binary from the api-rs image; `migrate` adopts the schema on first
   start.
2. Rollback is redeploying the previous release (Rails image and chart).
   This stays safe because the schema is unchanged and frozen for one
   release and scheduled task runs use Rails' idempotency keys. Browser
   sessions are not shared, so everyone signs in again (Decision 4). The `*_queue`/`*_cache`/`*_cable`
   databases are left in place for that window, so a rolled-back Rails
   worker still starts.

## Build history

The port was built in phases: a foundation, then per-area builders (secrets,
principals, identity, OAuth, automation, misc, the api-rs scheduled-task
runner, deployment), per-area verification against the Rails console, and
review. A differential harness drove the Rails and Rust consoles side by
side during the port. It is retired: it is kept on branch `console-rs/diff`
for local use only, and nothing in the repository depends on it. After the
simplification pass (Decisions 4 and 5, revised) the Rust console is
verified by its own tests, not by parity with Rails.

## Decisions

1. Topology: api-rs workspace and image, separate `console` and
   `console-worker` Deployments, same Service.
2. Sessions: own secure cookie and CSRF design (Decision 4, revised).
3. Scheduled tasks: the table stays in the console; api-rs owns timing on
   Absurd through claim/result endpoints.
4. Delivery: Rails is deleted in the same change.
5. No byte-for-byte parity; keep external contracts only (Decision 5, revised).
6. Reviews by `gpt-6-astra` (`openai-codex`).
