# Telegrambot Guide

## Role

Telegrambot is a long-polling Telegram Bot API client with a small health server. It turns
allowed replies, addressed commands, and DMs into durable Centaur sessions and renders
progress and answers back into the originating chat/topic.

Key modules are `src/poller.ts` (long-poll controller), `src/inbox.ts` +
`src/ownership.ts` + `src/migrations.ts` (durable receipt ledger and fenced
ownership), `src/telegram-allowlist.ts`, `src/telegram-threading.ts`,
`src/telegram-render.ts` (entity-aware HTML chunking), `src/telegram-narrator.ts`,
`src/rate-limit.ts`, and `src/session-api.ts`. See `README.md` for platform setup
and behavior.

## Invariants

- Run one `getUpdates` consumer per bot token (single replica, `Recreate`). Fenced
  ownership in Postgres — not the replica count — is the correctness mechanism: every
  cursor update, inbox claim, and render transition must match the current
  `holder_id` + `generation` on an unexpired lease. A pod that cannot prove its lease
  stops polling and fails readiness.
- Receipt is separate from processing. A `getUpdates` batch is upserted and
  `receive_offset` advanced in one transaction; only a committed cursor may feed the
  next poll. Processing/rendering completion never gates the cursor. Update ids are
  not assumed contiguous.
- Accepted updates progress idempotently through
  `received → message_appended → execution_accepted → render_obligation_persisted → terminal`,
  keyed by stable `telegram:{chatId}:{messageId}` idempotency keys. Append success is
  not terminal; crashes at any handoff resume without duplicating executions.
  Ignored/rejected updates are terminal only with a durable reason.
- Access is fail-closed. Empty chat/user allowlists mean the bot is inert; DMs
  require per-user allowlisting (Telegram has no workspace boundary). Self-messages
  and other bots are rejected. Privacy mode is delivery minimization, not
  authorization — unrelated group traffic must still fail the trigger gate.
- One chat/topic maps to one stable typed thread key (`telegram:chat:…` /
  `telegram:private:…`, preserving `message_thread_id`). Same-thread updates stay
  FIFO; a follow-up during an active execution is steered via api-rs, never dropped
  and never a competing execution.
- Respect Telegram limits: ≤4096 chars per message with balanced entities across
  chunks, ~1 msg/s per chat, honor `429 retry_after`, throttle in-progress edits,
  and keep reactions/typing best-effort — cosmetic failures must not fail delivery.
- Terminal delivery is at-least-once via persisted render obligations; recovery may
  duplicate the final answer but must never re-execute the agent to redeliver it.
- `/live` reflects only local process health; `/ready` gates on database, schema
  version, ownership, webhook reconciliation, and poll freshness. Telegram outages
  fail readiness, not liveness.
- Never log the bot token or unredacted Bot API URLs (the token is in the URL path).

## Validation

From the repository root:

```bash
pnpm --filter telegrambot run check:types
pnpm --filter telegrambot test
```

The unit suite uses fakes (including a fake Bot API HTTP server) and needs no Telegram
credential; inbox/ownership suites run against Postgres when `TELEGRAMBOT_TEST_DATABASE_URL`
is set and are skipped otherwise. Add focused coverage for allowlist, threading, poller
receipt/recovery, narration, rendering, and session API changes.

Plain textual `@botname` mention delivery under privacy mode is **not** covered by CI: it
requires a live acceptance test with an ordinary, non-admin, privacy-enabled bot. Until that
test passes, replies + addressed commands + DMs are the supported trigger contract.
