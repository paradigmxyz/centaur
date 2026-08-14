# Feishu Per-Message Delivery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every Feishu user message own a newly positioned response card and prevent stale or duplicate render streams from replacing that response with output from another execution.

**Architecture:** Keep the existing single recoverable Feishu delivery per development thread, but rotate it to a monotonically increasing generation for each user-triggered operation. Bind each generation to its execution or selection flow in Postgres, reject stale-generation and regressing-cursor writes, and share one in-process render promise per delivery generation.

**Tech Stack:** PostgreSQL migrations and triggers, Rust/sqlx/axum, TypeScript/Bun, Feishu Open Platform message APIs.

## Global Constraints

- Preserve unrelated uncommitted changes in `services/api-rs/crates/centaur-session-sqlx/src/development.rs`, `services/feishubot/src/bot.ts`, and `services/feishubot/test/bot-metrics.test.ts`.
- Never log or commit Feishu credentials, GitLab tokens, tenant identifiers, or private message content.
- Keep durable delivery ownership in `api-rs`; the Feishu bot remains a transport and renderer.
- Each ordinary message and `/projects` command must create a response below its triggering Feishu message.
- Only events from the execution bound to the current generation may wake that delivery.
- A stale generation or lower event cursor must never replace current durable delivery state.
- Do not deploy or restart Kubernetes workloads without a separate explicit authorization.

---

## File Map

- `services/api-rs/crates/centaur-session-sqlx/migrations/0058_feishu_delivery_generations.sql`: generation columns, target bindings, backfill, and scoped wake triggers.
- `services/api-rs/crates/centaur-session-core/src/development.rs`: public durable delivery and record-write fields.
- `services/api-rs/crates/centaur-session-sqlx/src/development.rs`: transactional generation rotation, target-aware loading, and stale-write guards.
- `services/api-rs/crates/centaur-session-runtime/src/lib.rs`: forwards add-selection source-message identity.
- `services/api-rs/crates/centaur-api-server/src/types.rs`: accepts the generation write guard and optional `/projects` source message.
- `services/api-rs/crates/centaur-api-server/src/development.rs`: maps the HTTP request fields to runtime types.
- `services/api-rs/crates/centaur-api-server/src/client.rs`: keeps the internal Rust client compatible with the expanded request.
- `services/feishubot/src/session-api.ts`: TypeScript delivery shape and request bodies.
- `services/feishubot/src/bot.ts`: generation-aware replies, idempotency, stale-render termination, and render single-flight.
- `services/feishubot/src/render-recovery.ts`: isolates delivery failures and awaits actual reconciliation.
- `services/feishubot/test/session-api.test.ts`: request-contract coverage.
- `services/feishubot/test/bot-delivery.test.ts`: card placement and render single-flight coverage.
- `services/feishubot/test/render-recovery.test.ts`: recovery batch isolation coverage.

### Task 1: Durable Delivery Generation

**Files:**
- Create: `services/api-rs/crates/centaur-session-sqlx/migrations/0058_feishu_delivery_generations.sql`
- Modify: `services/api-rs/crates/centaur-session-core/src/development.rs`
- Modify: `services/api-rs/crates/centaur-session-sqlx/src/development.rs`
- Test: `services/api-rs/crates/centaur-session-sqlx/src/development.rs`

**Interfaces:**
- Produces: `FeishuDelivery.delivery_generation: i32`.
- Produces: `RecordFeishuDelivery.expected_delivery_generation: i32`.
- Produces: `PgSessionStore::create_add_repository_selection(&ThreadKey, &str, bool, Option<&str>)`.
- Produces: delivery rows directly bound to `execution_id` and `selection_flow_id`.

- [ ] **Step 1: Extend the SQLx recovery test with generation and target assertions**

Add these assertions and stale-write cases to `feishu_delivery_recovery_versions_every_new_render_obligation`:

```rust
assert_eq!(initial.delivery_generation, 0);
assert_eq!(initial.execution_id.as_deref(), Some(accepted.execution_id.as_str()));
assert_eq!(
    initial.selection_flow_id.as_deref(),
    Some(accepted.selection_flow_id.as_str())
);

// Include this field in every RecordFeishuDelivery in this test.
expected_delivery_generation: initial.delivery_generation,

assert_eq!(continuation_wakeup.delivery_generation, 1);
assert!(continuation_wakeup.message_id.is_none());
assert_eq!(continuation_wakeup.last_event_cursor, 0);
assert_eq!(
    continuation_wakeup.execution_id.as_deref(),
    Some(continued.execution_id.as_str())
);
assert!(continuation_wakeup.selection_flow_id.is_none());

assert!(matches!(
    store.record_feishu_delivery(&RecordFeishuDelivery {
        thread_key: accepted.thread_key.clone(),
        message_id: "om-stale-card".to_owned(),
        last_event_cursor: 0,
        expected_desired_version: continuation_wakeup.desired_version,
        expected_delivery_generation: 0,
    }).await,
    Err(crate::SessionStoreError::DevelopmentConflict { .. })
));
```

After recording cursor `10`, assert that cursor `9` conflicts. Append an unscoped event and an event from `accepted.execution_id`, then assert neither changes `desired_version` after the delivery is bound to `continued.execution_id`:

```rust
let rendered = store.record_feishu_delivery(&RecordFeishuDelivery {
    thread_key: accepted.thread_key.clone(),
    message_id: "om-card-2".to_owned(),
    last_event_cursor: 10,
    expected_desired_version: continuation_wakeup.desired_version,
    expected_delivery_generation: continuation_wakeup.delivery_generation,
}).await.unwrap();
assert!(store.record_feishu_delivery(&RecordFeishuDelivery {
    thread_key: accepted.thread_key.clone(),
    message_id: "om-card-2".to_owned(),
    last_event_cursor: 9,
    expected_desired_version: rendered.desired_version,
    expected_delivery_generation: rendered.delivery_generation,
}).await.is_err());
store.append_event(&accepted.thread_key, None, "session.stdout_eof", json!({})).await.unwrap();
store.append_event(
    &accepted.thread_key,
    Some(&accepted.execution_id),
    "session.execution_completed",
    json!({}),
).await.unwrap();
assert_eq!(
    store.get_feishu_delivery(&accepted.thread_key).await.unwrap().desired_version,
    rendered.desired_version
);
```

- [ ] **Step 2: Run the focused SQLx test and verify it fails**

Run:

```bash
cargo test --manifest-path services/api-rs/Cargo.toml -p centaur-session-sqlx feishu_delivery_recovery_versions_every_new_render_obligation -- --nocapture
```

Expected: compilation fails because generation fields and the fourth add-selection argument do not exist.

- [ ] **Step 3: Add migration `0058_feishu_delivery_generations.sql`**

Create the columns, backfill current targets, and replace both broad triggers:

```sql
alter table feishu_deliveries
    add column delivery_generation integer not null default 0,
    add column execution_id text references session_executions(execution_id) on delete set null,
    add column selection_flow_id text references development_selection_flows(selection_flow_id) on delete set null,
    add constraint feishu_deliveries_generation_nonnegative
        check (delivery_generation >= 0);

update feishu_deliveries delivery
   set execution_id = (
           select execution.execution_id
             from session_executions execution
            where execution.thread_key = delivery.thread_key
            order by execution.created_at desc, execution.execution_id desc
            limit 1
       ),
       selection_flow_id = (
           select flow.selection_flow_id
             from development_selection_flows flow
             join session_workspaces workspace using (workspace_id)
            where workspace.thread_key = delivery.thread_key
              and flow.state = 'pending'
            order by flow.created_at desc, flow.selection_flow_id desc
            limit 1
       );

create or replace function wake_feishu_delivery_for_session_event()
returns trigger language plpgsql as $$
begin
    if new.execution_id is not null then
        update feishu_deliveries
           set desired_version = desired_version + 1,
               state = 'pending', failure_code = null, updated_at = now()
         where thread_key = new.thread_key
           and execution_id = new.execution_id;
    end if;
    return new;
end;
$$;

create or replace function wake_feishu_delivery_for_selection()
returns trigger language plpgsql as $$
begin
    update feishu_deliveries
       set desired_version = desired_version + 1,
           state = 'pending', failure_code = null, updated_at = now()
     where selection_flow_id = new.selection_flow_id;
    return new;
end;
$$;
```

- [ ] **Step 4: Extend Rust delivery types and guarded writes**

Add `delivery_generation` to `FeishuDelivery` and `expected_delivery_generation` to `RecordFeishuDelivery`. Change the record SQL predicate to:

```sql
where thread_key = $1
  and desired_version = $4
  and delivery_generation = $5
  and $3 >= last_event_cursor
```

Bind `record.expected_delivery_generation` as `$5`. Select the stored target columns, exposing `selection_flow_id` only while its flow is pending:

```sql
case when selection.state = 'pending' then delivery.selection_flow_id end as selection_flow_id,
delivery.execution_id,
```

Use a `left join development_selection_flows selection on selection.selection_flow_id = delivery.selection_flow_id` in `load_feishu_delivery`.

- [ ] **Step 5: Rotate the delivery transactionally for follow-up messages**

Move the Feishu delivery update after the new execution insert and replace it with:

```sql
update feishu_deliveries
   set delivery_generation = delivery_generation + 1,
       desired_version = desired_version + 1,
       state = 'pending', source_message_id = $2,
       message_id = null, last_event_cursor = 0,
       execution_id = $3, selection_flow_id = null,
       failure_code = null, lease_owner = null, lease_expires_at = null,
       updated_at = now()
 where thread_key = $1
```

Bind the new `execution_id` as `$3`. Initial delivery insertion stores both `execution_id` and `selection_flow_id`.

- [ ] **Step 6: Rotate `/projects` delivery generations**

Change the store signature to accept `source_message_id: Option<&str>`. After finding or inserting the selection flow, bind it to the delivery. When a source message is present, rotate card identity:

```sql
update feishu_deliveries
   set delivery_generation = delivery_generation +
           case when $3::text is not null
                  and source_message_id is distinct from $3
                then 1 else 0 end,
       desired_version = desired_version + 1,
       source_message_id = coalesce($3, source_message_id),
       message_id = case when $3::text is not null
                              and source_message_id is distinct from $3
                         then null else message_id end,
       last_event_cursor = case when $3::text is not null
                                     and source_message_id is distinct from $3
                                then 0 else last_event_cursor end,
       execution_id = null, selection_flow_id = $2,
       state = 'pending', failure_code = null,
       lease_owner = null, lease_expires_at = null, updated_at = now()
 where thread_key = $1
```

- [ ] **Step 7: Run focused and crate tests**

Run:

```bash
cargo test --manifest-path services/api-rs/Cargo.toml -p centaur-session-sqlx feishu_delivery -- --nocapture
cargo test --manifest-path services/api-rs/Cargo.toml -p centaur-session-sqlx development::tests -- --nocapture
```

Expected: all selected tests pass.

### Task 2: HTTP And Client Contract

**Files:**
- Modify: `services/api-rs/crates/centaur-api-server/src/types.rs`
- Modify: `services/api-rs/crates/centaur-api-server/src/development.rs`
- Modify: `services/api-rs/crates/centaur-api-server/src/client.rs`
- Modify: `services/api-rs/crates/centaur-session-runtime/src/lib.rs`
- Modify: `services/feishubot/src/session-api.ts`
- Test: `services/feishubot/test/session-api.test.ts`

**Interfaces:**
- Consumes: `RecordFeishuDelivery.expected_delivery_generation` from Task 1.
- Consumes: the four-argument `create_add_repository_selection` store method from Task 1.
- Produces: `FeishuSessionApi.createAddSelection(threadKey, principalId, sourceMessageId)`.
- Produces: `FeishuSessionApi.recordDelivery(..., expectedDeliveryGeneration)`.

- [ ] **Step 1: Write failing TypeScript request-contract assertions**

Add a test which invokes both methods and checks exact bodies:

```ts
it('carries the source message and generation guards for durable delivery', async () => {
  const calls: Array<{ url: string; body: unknown }> = []
  const api = new FeishuSessionApi({
    baseUrl: 'http://api-rs:8080/',
    fetch: async (input, init) => {
      calls.push({
        url: String(input),
        body: init?.body ? JSON.parse(String(init.body)) : undefined
      })
      return response({})
    }
  })
  await api.createAddSelection('development:1', 'feishu:tenant:ou-1', 'om-projects')
  await api.recordDelivery('development:1', 'om-card', 42, 7, 3)
  expect(calls).toEqual([
    {
      url: 'http://api-rs:8080/api/development/sessions/development%3A1/repositories',
      body: {
        requested_by_principal_id: 'feishu:tenant:ou-1',
        source_message_id: 'om-projects'
      }
    },
    {
      url: 'http://api-rs:8080/api/development/feishu/deliveries/development%3A1',
      body: {
        message_id: 'om-card',
        last_event_cursor: 42,
        expected_desired_version: 7,
        expected_delivery_generation: 3
      }
    }
  ])
})
```

- [ ] **Step 2: Run the client test and verify it fails**

Run:

```bash
pnpm --filter feishubot test -- session-api.test.ts
```

Expected: TypeScript reports excess method arguments or the expected request fields are absent.

- [ ] **Step 3: Thread request fields through Rust API layers**

Add these fields:

```rust
pub struct CreateAddRepositorySelectionRequest {
    pub requested_by_principal_id: String,
    #[serde(default)]
    pub source_message_id: Option<String>,
}

pub struct RecordFeishuDeliveryRequest {
    pub message_id: String,
    pub last_event_cursor: i64,
    pub expected_desired_version: i32,
    pub expected_delivery_generation: i32,
}
```

Map both fields in `development.rs`. Extend the runtime add-selection method with `source_message_id: Option<&str>` and pass it to the store. Keep the general Rust client source-neutral by serializing `source_message_id: None`.

- [ ] **Step 4: Extend the TypeScript API client**

Add `delivery_generation: number` to `FeishuDelivery`. Send `source_message_id` from `createAddSelection`, and add `expected_delivery_generation` to `recordDelivery`:

```ts
async createAddSelection(threadKey: string, principalId: string, sourceMessageId: string) {
  return this.#json(
    'add projects',
    `api/development/sessions/${encodeURIComponent(threadKey)}/repositories`,
    {
      method: 'POST',
      body: JSON.stringify({
        requested_by_principal_id: principalId,
        source_message_id: sourceMessageId
      })
    }
  )
}
```

- [ ] **Step 5: Run API and client validation**

Run:

```bash
cargo test --manifest-path services/api-rs/Cargo.toml -p centaur-api-server development -- --nocapture
pnpm --filter feishubot test -- session-api.test.ts
pnpm --filter feishubot run check:types
```

Expected: all commands pass.

### Task 3: New Card Per Triggering Message

**Files:**
- Modify: `services/feishubot/src/bot.ts`
- Create: `services/feishubot/test/bot-delivery.test.ts`

**Interfaces:**
- Consumes: `FeishuDelivery.delivery_generation` from Task 2.
- Consumes: generation-aware `recordDelivery` and source-aware `createAddSelection` from Task 2.
- Produces: `renderExecutionOnce(delivery, messageId)` single-flight entry point.

- [ ] **Step 1: Write a failing new-message placement test**

Use a direct-message event whose accepted delivery has `source_message_id: 'om-user-2'`, `message_id: null`, and `delivery_generation: 2`. Assert:

```ts
expect(replies).toEqual([{
  sourceMessageId: 'om-user-2',
  inThread: false,
  idempotencyKey: 'fdl_1-2'
}])
expect(updatedMessageIds).not.toContain('om-card-1')
expect(recordedGenerations).toEqual([2])
```

The API stub returns an empty terminal event so the background renderer finishes without hanging.

- [ ] **Step 2: Write a failing `/projects` source-message test**

Send a normalized `/projects` event and assert the API call is exactly:

```ts
expect(addSelectionCalls).toEqual([{
  threadKey: 'development:1',
  principalId: 'feishu:tenant-1:ou-user-1',
  sourceMessageId: 'om-projects'
}])
```

- [ ] **Step 3: Run the bot test and verify it fails**

Run:

```bash
pnpm --filter feishubot test -- bot-delivery.test.ts
```

Expected: the old bot updates an existing card or omits the source message and generation.

- [ ] **Step 4: Make card creation generation-aware**

Pass `message.messageId` to `createAddSelection`. In `upsertSessionCard` and `renderDeliveryCard`, use this idempotency key when replying:

```ts
const idempotencyKey = `${delivery.delivery_id}-${delivery.delivery_generation}`
messageId = await this.options.renderer.replyCard(
  sourceMessageId,
  card,
  inThread,
  idempotencyKey
)
```

Pass `delivery.delivery_generation` through every call to the private `recordDelivery` wrapper. When the wrapper returns `false`, stop that render path because a newer generation owns the thread.

- [ ] **Step 5: Run the bot placement tests**

Run:

```bash
pnpm --filter feishubot test -- bot-delivery.test.ts bot-metrics.test.ts
```

Expected: both files pass, including the pre-existing execution-failure rendering test.

### Task 4: Single-Flight Rendering And Recovery Isolation

**Files:**
- Modify: `services/feishubot/src/bot.ts`
- Modify: `services/feishubot/src/render-recovery.ts`
- Modify: `services/feishubot/test/bot-delivery.test.ts`
- Create: `services/feishubot/test/render-recovery.test.ts`

**Interfaces:**
- Consumes: generation-bound delivery snapshots from Task 3.
- Produces: one active `renderExecution` promise per `thread_key`, generation, and execution ID.
- Produces: recovery that continues after a per-delivery failure.

- [ ] **Step 1: Write a failing single-flight test**

Make `streamEvents` increment a counter, wait on a deferred promise, and then yield a terminal event. Call `reconcileDelivery('development:1')` twice concurrently and assert:

```ts
const first = bot.reconcileDelivery('development:1')
const second = bot.reconcileDelivery('development:1')
await Bun.sleep(10)
expect(streamCount).toBe(1)
releaseStream()
await Promise.all([first, second])
```

- [ ] **Step 2: Write a failing recovery isolation test**

Run recovery with pending keys `['development:bad', 'development:good']`. Make the first reconciliation throw and the second call `recovery.stop()`. Assert both keys were attempted in order and both failure and success metrics were recorded.

- [ ] **Step 3: Run both focused tests and verify they fail**

Run:

```bash
pnpm --filter feishubot test -- bot-delivery.test.ts render-recovery.test.ts
```

Expected: two streams are created and the recovery loop skips the second delivery after the first throws.

- [ ] **Step 4: Add the shared render promise map**

Add:

```ts
private readonly activeRenders = new Map<string, Promise<void>>()

private renderExecutionOnce(
  delivery: FeishuDelivery,
  messageId: string
): Promise<void> {
  const executionId = delivery.execution_id
  if (!executionId) return Promise.resolve()
  const key = `${delivery.thread_key}:${delivery.delivery_generation}:${executionId}`
  const active = this.activeRenders.get(key)
  if (active) return active
  const created = this.renderExecution(
    messageId,
    delivery.thread_key,
    executionId,
    delivery.initiator_principal_id,
    delivery.last_event_cursor,
    delivery.delivery_generation
  ).finally(() => {
    if (this.activeRenders.get(key) === created) this.activeRenders.delete(key)
  })
  this.activeRenders.set(key, created)
  return created
}
```

Immediate event handlers detach this promise with `this.run`, while `reconcileDelivery` awaits it directly.

- [ ] **Step 5: Isolate recovery failures**

Replace the inner rethrow with logging and continue:

```ts
try {
  await this.reconciler.reconcileDelivery(threadKey)
  this.metrics.recordRecovery('succeeded')
} catch (error) {
  this.metrics.recordRecovery('failed')
  console.error('Feishu delivery reconciliation failed', {
    status: error instanceof Error ? error.name : 'unknown'
  })
}
```

Keep the outer catch for list failures only.

- [ ] **Step 6: Run the full Feishu suite**

Run:

```bash
pnpm --filter feishubot test
pnpm --filter feishubot run check:types
```

Expected: all tests and type checks pass.

### Task 5: Cross-Layer Verification

**Files:**
- Verify only; no new files expected.

**Interfaces:**
- Consumes: all contracts from Tasks 1-4.
- Produces: evidence that the migration, Rust API, and Feishu transport agree.

- [ ] **Step 1: Format only affected source files**

Run:

```bash
rustfmt --edition 2024 \
  services/api-rs/crates/centaur-session-core/src/development.rs \
  services/api-rs/crates/centaur-session-sqlx/src/development.rs \
  services/api-rs/crates/centaur-session-runtime/src/lib.rs \
  services/api-rs/crates/centaur-api-server/src/types.rs \
  services/api-rs/crates/centaur-api-server/src/development.rs \
  services/api-rs/crates/centaur-api-server/src/client.rs
```

Expected: the formatter succeeds without touching unrelated files.

- [ ] **Step 2: Run affected Rust and TypeScript checks**

Run:

```bash
cargo test --manifest-path services/api-rs/Cargo.toml -p centaur-session-sqlx feishu_delivery -- --nocapture
cargo test --manifest-path services/api-rs/Cargo.toml -p centaur-api-server development -- --nocapture
pnpm --filter feishubot test
pnpm --filter feishubot run check:types
```

Expected: every command passes.

- [ ] **Step 3: Check the final diff without staging unrelated work**

Run:

```bash
git diff --check
git status --short
git diff -- services/api-rs/crates/centaur-session-core/src/development.rs \
  services/api-rs/crates/centaur-session-sqlx/src/development.rs \
  services/api-rs/crates/centaur-session-runtime/src/lib.rs \
  services/api-rs/crates/centaur-api-server/src/types.rs \
  services/api-rs/crates/centaur-api-server/src/development.rs \
  services/api-rs/crates/centaur-api-server/src/client.rs \
  services/feishubot/src/session-api.ts services/feishubot/src/bot.ts \
  services/feishubot/src/render-recovery.ts services/feishubot/test
```

Expected: no whitespace errors; unrelated GitLab and workspace-failure changes remain present and unstaged.

- [ ] **Step 4: Stop before deployment**

Report the passing evidence and request explicit authorization before running `just deploy`, restarting pods, or sending a real Feishu validation message.
