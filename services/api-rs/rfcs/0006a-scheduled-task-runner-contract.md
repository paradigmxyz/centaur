# RFC 0006a: Scheduled-task runner contract

Status: accepted (implementation contract for RFC 0006 Decision 3)

This is the wire contract between the api-rs scheduled-task runner and the
console. RFC 0006 Decision 3 is the design; this document pins the endpoints,
tokens, payloads, status codes, and retry and replay rules both sides
implement. The oracle for every console-side write is the Rails code at the
RFC 0006 merge base: `ScheduledTaskSchedulerJob`, `ScheduledTaskRunJob`,
`ScheduledTask#api_input`, and `ConsoleUserPrincipalProvisioner`.

## Roles

- The **console** owns the `scheduled_tasks` rows: CRUD, validation,
  `next_run_at` (computed with `centaur_console_db::schedule`, on `croner`),
  `last_enqueued_at`, `last_run_id`, `last_run_at`, `last_error`, principal
  provisioning, and the Slack delivery policy. Every write to its database is
  made by the console.
- **api-rs** owns *when* a run happens. It reads `scheduled_tasks` through its
  existing read-only console database connection, keeps one durable Absurd
  `scheduled_task.tick` task per pending occurrence, and spawns the
  `console_workflow` run itself.

```
console write ──notify──▶ api-rs POST /api/admin/scheduled-tasks/{oid}/reconcile
                                │ (and every reconcile interval)
                                ▼
                     Absurd scheduled_task.tick (sleep_until scheduled_for)
                                │
            claim ──▶ console POST /internal/v1/scheduled_tasks/{oid}/claim
            spawn ──▶ console_workflow run (standard workflow queue)
           report ──▶ console POST /internal/v1/scheduled_tasks/{oid}/result
            chain ──▶ next tick at the console's next_run_at
```

## Time format

`scheduled_for` and `next_run_at` are RFC 3339 UTC strings with a `Z`
suffix. Seconds are always present; fractional seconds are present (up to
microseconds, the precision of the `timestamp(6)` columns) only when non-zero:
`2026-08-19T12:00:00Z`, `2026-08-19T12:00:00.250000Z`. Cron occurrences are
whole minutes, so in practice there are no fractions. Equality checks compare
instants at microsecond precision, never strings. Both sides accept any RFC
3339 offset on input and convert to UTC.

## Idempotency keys

| Key | Queue | Format |
| --- | --- | --- |
| Scheduled tick | `centaur_scheduled_tasks` | `scheduled-task-tick:{id}:{scheduled_for}` |
| Manual tick | `centaur_scheduled_tasks` | `scheduled-task-tick:{id}:manual:{scheduled_for}` |
| Workflow run | `centaur_workflows` (standard) | `scheduled-task:{id}:{scheduled_for}` |

`{id}` is the numeric `scheduled_tasks.id` (not the oid). `{scheduled_for}` in
keys is formatted exactly as Rails' `Time.iso8601(s).utc.iso8601`:
`%Y-%m-%dT%H:%M:%SZ`, whole seconds, fractions truncated.

- The run key is byte-identical to `ScheduledTaskRunJob#idempotency_key`, so a
  run Rails created before cutover (or after a rollback) for an occurrence is
  returned, not duplicated, when api-rs spawns it, and vice versa.
- Manual ticks get a separate `manual:` segment so a "run now" in the same
  second as a scheduled occurrence cannot be absorbed into (and suppress the
  claim of) the scheduled tick. Both still spawn the same run key, which is
  what Rails does too: a manual run and a scheduled run for the same second are
  one run.
- Absurd keeps a key for the life of the task row, completed or not, so
  reconcile and chaining never create a second tick for one occurrence.

## Authentication

### console -> api-rs

The existing console service JWT (`ApiServer::Jwt.encode_for_console_service`):
HS256 with `CENTAUR_JWT_SIGNING_SECRET`, `iss: centaur-console`, `aud:
centaur-api`, `sub: centaur-console`, `token_use: console_service`, `iat`,
`exp`. The static api-rs admin key is also accepted (operators). Principal
JWTs, ingress keys, and API client tokens get 403.

### api-rs -> console

The api-rs service token the console's `/internal/v1/*` namespace verifies
(`centaur_console_auth::jwt::internal`), the mirror image of the console
service JWT:

```json
{
  "iss": "centaur-api",
  "aud": "centaur-console",
  "iat": 1787140800,
  "exp": 1787144400,
  "sub": "centaur-api",
  "token_use": "api_service"
}
```

- HS256, signed with `CENTAUR_JWT_SIGNING_SECRET` (already shared by the
  console and api-rs), header `{"typ":"JWT","alg":"HS256"}`.
- A windowed token like the console's own (`CentaurJwt::WindowedToken`): a
  15-minute rotation window offset by `crc32("centaur-api")`, `exp = iat +
  3600`. api-rs mints it at its clock's time; the console verifies the
  signature, `iss`, `aud`, `exp`, and the exact `sub` and `token_use` at its
  own clock's time.
- Sent as `Authorization: Bearer <jwt>` with `Accept: application/json` and
  `Content-Type: application/json`.
- Accepted only on `/internal/v1/*`. It cannot be replayed against api-rs,
  which requires `iss: centaur-console` and `aud: centaur-api` on console
  JWTs.
- Missing or invalid -> `401 {"error":"invalid or missing service token"}`.

(The first version of this contract specified a separate `centaur-api-rs` /
`scheduled_task_runner` token; the console foundation had already shipped
this one for the internal namespace, so the runner uses it.)

## Console endpoints (called by api-rs)

Base URL: api-rs's `IRON_CONTROL_URL` (the console Service URL api-rs already
uses). Unknown oids, malformed oids, and rows that no longer exist all return
`404 {"error":"scheduled task not found"}`. A request body that is not a JSON
object with a valid `scheduled_for` returns `422 {"error":"<message>"}`.

### `POST /internal/v1/scheduled_tasks/{oid}/claim`

Request:

```json
{ "scheduled_for": "2026-08-19T12:00:00Z", "manual": false }
```

`manual` defaults to `false`.

The console runs one transaction holding `SELECT ... FOR UPDATE` on the row
(`ScheduledTask.lock.find`), and evaluates in this order:

1. Row missing -> `404`.
2. `enabled` is false -> `skipped`/`task_disabled`. No writes.
   (Parity: the scheduler only selects enabled tasks, and
   `ScheduledTaskRunJob` returns silently for a disabled task, including a
   manual run.)
3. Scheduled claims only (`manual: false`):
   - `next_run_at == scheduled_for`: **claim**. Set `last_enqueued_at =
     scheduled_for` and `next_run_at` to the first occurrence after
     `max(now, scheduled_for)`, where `now` is the console's clock (the
     maximum keeps a console clock slightly behind the runner's from handing
     the same occurrence out again, which would stop the chain for good).
     `updated_at` is touched. Occurrences missed while the runner was down
     are skipped. The task need not be due yet by the console's clock: api-rs
     only claims after waking at `scheduled_for`, and refusing a claim a few
     milliseconds early because of clock skew would drop the occurrence.
     Only these columns are written; edits of a task's user-editable fields
     never write them, and a schedule edit rewrites `next_run_at` under the
     same row lock.
   - else `last_enqueued_at == scheduled_for`: **replay** of a claim that
     already committed (api-rs crashed before checkpointing the response).
     No scheduling writes; continue to step 4 as a claim. The console only
     remembers the latest claim, so api-rs serializes the claims of one task:
     a tick does not claim while an earlier scheduled tick of the same task
     is still open and has not checkpointed its own `claim` step.
   - else -> `skipped`/`not_due`. No writes. The tick is stale: the task was
     rescheduled, or this occurrence was already claimed and superseded.
4. Build `api_input` (`ScheduledTask#api_input`): check the delivery
   destination, provision the execution principal
   (`ConsoleUserPrincipalProvisioner` for an author-owned task), and build the
   payload. A `StandardError` here (for example
   `DeliveryDestinationUnavailable`) does **not** roll back step 3: the
   console records `last_error = e.message` with `updated_at = now`
   (`update_columns`, as `ScheduledTaskRunJob`'s rescue does) and answers
   `failed`. Errors that are not about the task (database connection loss)
   roll the whole transaction back and return 5xx.

`200` response, every key always present:

```json
{
  "outcome": "claimed",
  "reason": null,
  "error": null,
  "api_input": {
    "prompt": "Summarize open incidents.",
    "principal": "console-user-ada-example-com-2f1c0e4a9b7d",
    "channel": "C0123456789",
    "slack_user_id": "U0123456789",
    "scheduled_task_id": "tsk_2mZbQ7Xa",
    "scheduled_task_name": "Incident summary"
  },
  "enabled": true,
  "next_run_at": "2026-08-19T13:00:00Z"
}
```

| `outcome` | `reason` | `error` | `api_input` | Console wrote |
| --- | --- | --- | --- | --- |
| `claimed` | `null` | `null` | object | step 3 (unless replay or manual) |
| `skipped` | `task_disabled` or `not_due` | `null` | `null` | nothing |
| `failed` | `delivery_destination_unavailable`, `principal_provisioning_failed`, or `api_input_failed` | message (also stored in `last_error`) | `null` | step 3 plus `last_error` |

`api_input` is `ScheduledTask#api_input` after `.compact`: absent keys are
omitted, not `null` (`slack_user_id` is absent for channel-owned tasks and
when the author has no unambiguous Slack identity). api-rs passes it through
unchanged as the workflow input. `enabled` and `next_run_at` are the row's
values after the transaction; `next_run_at` is `null` when the task has no
schedule.

### `POST /internal/v1/scheduled_tasks/{oid}/result`

Request, exactly one of `run_id` or `error`:

```json
{ "scheduled_for": "2026-08-19T12:00:00Z", "manual": false, "run_id": "0193..." }
{ "scheduled_for": "2026-08-19T12:00:00Z", "manual": false, "error": "internal server error" }
```

- `run_id`: `update!(last_run_id: run_id, last_run_at: now, last_error: nil)`
  as `ScheduledTaskRunJob` does after `create_workflow_run` succeeds.
- `error`: `update_columns(last_error: error, updated_at: now)`, as its rescue
  does when `create_workflow_run` raises.
- Both are idempotent apart from re-stamping `last_run_at`/`updated_at`.
  `scheduled_for` and `manual` are informational (logs); the console does not
  compare them with the row.

`200` response, keys always present:

```json
{ "enabled": true, "next_run_at": "2026-08-19T13:00:00Z" }
```

`404` when the row is gone. The console does not check `enabled` here: a run
that was spawned is recorded even if the task was disabled meanwhile, as
`ScheduledTaskRunJob` does.

## api-rs endpoints (called by the console)

Both require the console service JWT (above). Unknown or malformed oids
return `404 {"ok":false,"error":"scheduled task not found"}`. When the runner
is not enabled, or on a database error, both return `503
{"ok":false,"error":"internal server error"}` (api-rs never echoes 5xx
details; the log says which). Other callers get api-rs's usual `401`/`403`.

### `POST /api/admin/scheduled-tasks/{oid}/reconcile`

No body. The console calls it after a create, update, enable, disable, or
delete commits. api-rs reads the row and ensures one tick for its current
`next_run_at` (see Reconcile). Response `200`:

```json
{
  "ok": true,
  "scheduled_task_id": "tsk_2mZbQ7Xa",
  "state": "scheduled",
  "next_run_at": "2026-08-19T13:00:00Z",
  "tick": { "task_id": "0193...", "action": "created" },
  "cancelled": 0
}
```

- `state`: `scheduled` (enabled with a `next_run_at`), `unscheduled` (disabled
  or no `next_run_at`; `next_run_at` and `tick` are `null`), or `deleted` (row
  gone; `tick` is `null`).
- `tick.action`: `created`, `existing` (already pending, running, sleeping,
  completed, or cancelled), or `rearmed` (it had failed; see Retry policy).
- `cancelled`: ticks cancelled because the row is gone. Only deleted tasks'
  ticks are cancelled (see Stale ticks).

The call is a hint. A console that cannot reach api-rs loses nothing: the
periodic reconcile reaches the same state within its interval.

### `POST /api/admin/scheduled-tasks/{oid}/run`

No body. The console calls it from its console and sandbox `POST .../run`
endpoints after its own `enabled` check (the console keeps the `422
task_disabled` / `422 scheduled task is disabled` responses). api-rs spawns a
manual tick with `scheduled_for = now` truncated to whole seconds (Rails'
`Time.current.iso8601`). Response `202`:

```json
{
  "ok": true,
  "queued": true,
  "scheduled_task_id": "tsk_2mZbQ7Xa",
  "scheduled_for": "2026-08-19T12:05:00Z",
  "tick": { "task_id": "0193...", "created": true }
}
```

The console maps a `202` to its `202 {data: ..., queued: true}` and anything
else (including connection failures) to its `503 {"error":"scheduled task
run could not be queued"}`, which is what Rails returns when `perform_later`
fails. api-rs does not read the row for `run`; the claim re-checks `enabled`.

## Tick task

- Queue `centaur_scheduled_tasks`, task name `scheduled_task.tick`, params:

  ```json
  { "scheduled_task_id": 42, "scheduled_task_oid": "tsk_2mZbQ7Xa",
    "scheduled_for": "2026-08-19T12:00:00Z", "manual": false }
  ```

- Steps, each an Absurd checkpoint, so a step that completed is never redone
  after a crash or retry:
  1. `sleep_until(scheduled_for)`.
  2. `claim`. Before calling the console, api-rs reads the row read-only: if it
     is gone, disabled, or (scheduled ticks) neither its `next_run_at` nor
     (replay) its `last_enqueued_at` equals `scheduled_for`, the step completes
     as a skip (`task_not_found`, `task_disabled`, `not_due`) without an HTTP
     call. Otherwise it calls `claim` and checkpoints the response. The
     console's compare-and-set is authoritative; the pre-check only saves
     calls for stale ticks.
  3. `spawn` (only for `claimed`): `console_workflow` on the standard workflow
     queue with the run key, `max_attempts: 3`, `input: api_input`, the
     server's default harness, and no `harness_type`, as `POST
     /api/workflows/runs` does for the console (including the workflow
     enablement policy and the workflow's declared starters, checked as the
     `console` caller class). Retries follow Rails' `retry_on
     CentaurApiClient::Error, wait: :polynomially_longer, attempts: 5`: each
     attempt is its own checkpoint (`spawn`, `spawn#2`, ...); after a
     failure the error is reported (`result` with `error`) and the tick
     sleeps durably (`spawn-retry`) for `executions**4 + 2` seconds (3, 18,
     83, 258 s; Rails' random jitter is not reproduced) before the next
     attempt. After the 5th failure, or at once for an error Rails did not
     retry (a transport error, or a 2xx answer without `run_id`), the step
     completes with the error. Spawn retries never consume tick attempts.
  4. `report`: `result` with the `run_id`, or with the final error.
  5. `chain` (scheduled ticks only): ensure a tick for the `next_run_at` in
     the `result` response (or the `claim` response when nothing was
     reported). No chain when it is `null`, the task is disabled or gone, or
     it is not after `scheduled_for`.
- Result (task `completed` payload), for operators:

  ```json
  { "scheduled_task_id": 42, "scheduled_for": "...", "manual": false,
    "outcome": "spawned", "reason": null, "error": null,
    "run_id": "...", "run_created": true, "next_tick": "..." }
  ```

  `outcome`: `spawned`, `spawn_failed`, `skipped`, or `failed`.

### Spawn error messages

`last_error` for a failed spawn is what Rails stored from `CentaurApiClient`:
the `error` field of the response `POST /api/workflows/runs` would have
returned. For 4xx-class errors that is the message (for example `workflow
"console_workflow" is disabled` or `caller is not allowed to start workflow
"console_workflow"`); for 5xx-class errors it is `internal server error`.

## Retry policy

| Condition | api-rs behaviour |
| --- | --- |
| claim/result: connection error, timeout (20 s, as `CentaurApiClient`), any non-2xx status other than 404, a body that is not the documented JSON | Attempt fails; Absurd retries the tick. 4xx other than 404 means misconfiguration or a contract bug and is logged as an error, but is still retried so the occurrence fires once it is fixed. |
| claim: 404 | Terminal skip `task_not_found`; no chain. |
| result: 404 | Treated as reported; no chain. |
| claim `skipped` | Terminal skip; chain from `next_run_at`. |
| claim `failed` | Terminal (the console recorded `last_error`); no run; chain. |
| spawn error | Report `error`, durable sleep, retry; terminal after 5 spawn attempts (or a non-retryable error); chain. |

Tick retry strategy (console calls only): exponential, base 5 s, factor 2, capped at 300 s,
`max_attempts` 20 (about 1.3 hours). A tick that exhausts its attempts is
`failed`. Each reconcile pass re-arms every failed scheduled (not manual)
tick of a task that still exists, giving it one more attempt in place
(`absurd.retry_task`, which keeps its checkpoints, so completed steps are not
redone). An occurrence that could not be claimed because the console was down
for longer therefore still fires once the console is back, about once per
reconcile interval as Rails' minute scheduler would, and a tick that failed
after its claim still spawns and reports its run. A re-armed tick that has
gone stale skips at the pre-check. Failed manual ticks, and failed ticks of
deleted tasks, are left alone.

## Reconcile

On startup and every `SCHEDULED_TASK_RECONCILE_INTERVAL_SECS` (default 60),
api-rs reads

```sql
select id, next_run_at from scheduled_tasks
where enabled and next_run_at is not null
```

(Parity: Rails' `due` scope does not require a cron; a row with a
`next_run_at` is fired.) For each row it looks up the tick for
`scheduled-task-tick:{id}:{next_run_at}`: none -> spawn it; anything else ->
leave it (failed ticks were re-armed above; the single-task reconcile endpoint
re-arms its tick when it is failed). A due `next_run_at` (in the past) fires
immediately, so occurrences that came due while neither scheduler ran - the
cutover window, or a rollback and roll-forward - fire exactly once: the claim
is a compare-and-set and the run key matches Rails'.

### Stale ticks

- **Deleted task**: its non-terminal ticks are cancelled (by the reconcile
  endpoint and by the periodic pass). Ids are never reused, so nothing can
  need those keys again.
- **Disabled or rescheduled task**: its old tick is left sleeping. When it
  wakes, the pre-check or the claim skips it (`task_disabled`/`not_due`) and
  it ensures the tick for the current `next_run_at`. It is not cancelled
  because a cancelled tick's key can never be re-armed: re-enabling the task,
  or moving the schedule back, can produce the same `next_run_at` again, and
  that occurrence must still fire.

## Cutover and rollback

- **Cutover**: the Rails scheduler stops with the Rails worker. The first
  reconcile creates ticks from the Rails-set `next_run_at`, so due
  occurrences fire once. A run the Rails scheduler had claimed but its
  worker had not yet performed when Solid Queue was dropped is lost; there
  is no adoption.
- **Rollback**: the previous release's Rails scheduler resumes from the
  `next_run_at` api-rs's claims left. Leftover ticks have no worker. On
  roll-forward they wake, find `next_run_at` moved, and skip. Runs api-rs
  already spawned are deduplicated by the shared run key if Rails reaches
  the same occurrence. A run job Rails enqueued during the rollback and
  dropped on roll-forward is not recovered.

## Configuration (api-rs)

| Variable | Default | Meaning |
| --- | --- | --- |
| `SCHEDULED_TASK_RUNNER_ENABLED` | `false` | Run the tick worker and reconciler and serve the admin endpoints. Enable together with the Rust console; never while the Rails scheduler runs. |
| `SCHEDULED_TASK_RECONCILE_INTERVAL_SECS` | `60` | Periodic reconcile interval; `0` reconciles only at startup. |
| `SCHEDULED_TASK_WORKER_CONCURRENCY` | `2` | Concurrent ticks. |
| `IRON_CONTROL_URL` | existing | Console base URL for `claim`/`result`. |
| `IRON_CONTROL_DATABASE_URL` | existing | Read-only console database. |
| `CENTAUR_JWT_SIGNING_SECRET` | existing | Signs the api-rs service token; verifies console service JWTs. |
