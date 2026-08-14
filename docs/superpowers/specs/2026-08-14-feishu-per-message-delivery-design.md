# Feishu Per-Message Delivery Design

## Problem

Centaur currently stores one mutable Feishu delivery row per development thread.
Every later user message updates that row's `source_message_id`, but keeps the
original bot `message_id`. The bot therefore edits the first response card,
which appears above later user messages. Concurrent recovery streams can also
render an older execution into that shared card and overwrite the current
answer.

The expected behavior is:

- every ordinary user message receives a new response card below that message;
- `/projects` and repository-selection interactions follow the same placement
  rule;
- progress and final output update only the card created for that user message;
- recovery after a restart resumes the current delivery without duplicating or
  regressing it.

## Chosen Approach

Keep one recoverable current delivery per Feishu development thread, but give
it a monotonically increasing generation. Each user-triggered operation rotates
the delivery to a new generation in the same database transaction that creates
the operation:

- increment `delivery_generation`;
- replace `source_message_id` with the triggering Feishu message;
- clear `message_id` and `last_event_cursor`;
- bind the delivery directly to the new execution or selection flow;
- mark the delivery pending.

The old Feishu card remains in the conversation as the completed response to
the old user message. Only the current generation remains recoverable in
Postgres. This matches the existing serialized development-session model while
avoiding a broader append-only delivery-table redesign.

## Durable State And Guards

`feishu_deliveries` gains:

- `delivery_generation`, starting at zero;
- `execution_id`, identifying the execution allowed to wake and render the
  current generation;
- `selection_flow_id`, identifying a repository-selection obligation when no
  execution is active.

Every delivery write includes the expected generation and desired render
version. Before any visible render, a worker atomically claims a bounded durable
lease for that exact generation and desired version. Recording a render requires
the same lease owner, a live lease, and an event cursor greater than or equal to
the stored cursor. A worker holding an old generation or render obligation
therefore cannot mutate the current delivery after a new user message arrives.

The session-event wake trigger updates a Feishu delivery only when the event's
non-null `execution_id` matches the delivery's bound execution. Unscoped events
such as `session.stdout_eof`, and events from prior executions, cannot reopen a
completed response.

## Bot Rendering

After accepting a message, the bot loads the rotated delivery. Because the new
generation has no `message_id`, it replies to the triggering message and records
the returned Feishu message ID. The Feishu idempotency key combines the stable
delivery ID and generation, so a crash between sending and recording can retry
without intentionally reusing another generation's request ID.

All subsequent progress and final renders update that generation's recorded
message. Execution streaming and publication polling are single-flight per
delivery generation inside a bot process. Immediate handling and recovery share
the same entries, while the durable lease prevents a second bot process from
rendering the same obligation concurrently.

Execution recovery replays the bound execution from event zero to rebuild the
complete rendered answer, but only performs visible updates for events after the
durable cursor. This keeps recovered cards complete without regressing their
stored progress.

Recovery waits for the shared render task rather than returning immediately
after spawning another task. A failure for one pending delivery is recorded and
does not prevent other pending deliveries from being reconciled.

`/projects` passes its triggering Feishu message ID when creating an add-repo
selection flow. The API rotates the delivery generation and binds it to that
selection flow, causing the selection card to be created below the command. A
durable receipt identifies retries by both Feishu event ID and message ID. A new
command that reuses a pending selection increments the selection version so the
old card cannot mutate it.

The current schema intentionally retains only one recoverable generation. If a
second distinct `/projects` command arrives before the first command's card ID
has been recorded, the API returns a conflict instead of silently replacing the
first obligation. The bot replies below the second command and asks the user to
retry shortly.

## Failure Handling

- A stale generation receives a conflict from the record endpoint and stops
  rendering.
- A renderer that does not own the live durable lease cannot update a card or
  record delivery progress.
- A stale or duplicate cursor cannot regress durable delivery state.
- A restart finds a pending current generation and recreates or resumes only
  its card.
- A failed pending delivery does not abort recovery of unrelated deliveries.
- Platform retry uses the same source event and delivery generation, preserving
  idempotency.
- Migration backfill binds each existing delivery to the newer of its latest
  execution or latest selection operation, including terminal selection flows.

## Validation

Database tests will prove that a continuation rotates the generation, clears
the old card identity, binds the new execution, rejects stale-generation
writes, fences lease owners and desired versions, ignores unrelated or unscoped
session events, preserves `/projects` receipts, and backfills terminal selection
operations during upgrade.

Feishu bot tests will prove that a later message calls `replyCard` for a new
card rather than `updateCard` on the old one, `/projects` carries its source
message identity, duplicate reconciliation shares execution and publication
workers, concurrent unrecorded `/projects` commands receive an explicit retry
reply, and one failed delivery does not stop recovery of the remaining batch.

The affected Rust and TypeScript checks will run before a local deployment. A
real Feishu private-chat test will then verify ordering and content across two
consecutive prompts.
