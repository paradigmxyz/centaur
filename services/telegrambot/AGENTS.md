# Telegrambot Guide

## Role

Telegrambot is the Telegram chat ingress. It runs the Vercel Chat SDK with the
official `@chat-adapter/telegram` adapter and Postgres-backed Chat SDK state,
turns allowed Telegram messages into durable Centaur sessions, and renders
answers back into the originating chat.

Key modules are `src/telegram-policy.ts` (allowlists and triggers),
`src/index.ts` (handlers, forwarding, rendering, recovery),
`src/session-api.ts` (api-rs client), and `src/config.ts` (environment). The
adapter owns the transport: `getUpdates` polling with a durable checkpoint,
webhook secret-token verification and update claims, MarkdownV2 rendering,
reactions, and typing. Do not re-implement those here.

## Invariants

- Run one replica per bot token. Telegram allows a single `getUpdates`
  consumer, and render recovery assumes one process. Preserve the
  single-replica/Recreate deployment contract.
- Polling is the default and needs no public endpoint. Webhook mode is opt-in,
  requires `TELEGRAM_WEBHOOK_SECRET_TOKEN`, and is the only mode that exposes
  `POST /api/webhooks/telegram`. Never enable unverified webhooks.
- Access is fail-closed. Empty chat and user allowlists mean no work. DMs are
  allowed per user id; groups need both an allowlisted chat id and an
  allowlisted sender user id. Bot authors, `via_bot` relays,
  `sender_chat` identities, linked-channel auto-forwards, edits, and channel
  posts stay denied regardless of privacy mode.
- Group triggers are a reply to one of the bot's messages or the addressed
  `/ask@<bot>` command. A plain `@mention` triggers only with `observeGroups`
  (privacy mode off), matched by Telegram's mention entities; an unaddressed
  command never does.
- Never let the adapter persist thread history (`persistThreadHistory`): it
  stores every message before the allowlist runs. Message history is kept
  only by `observeGroups`, only after `isStorableTelegramMessage`.
- Thread keys are the adapter's `telegram:<chat_id>[:<topic_id>]`; api-rs
  derives Telegram principals from that shape. Business-mode keys are not
  supported; keep `businessMode` off.
- Polling redelivery and webhook retries must not create duplicate
  executions: keep the forwarded/executed message-id dedupe and the
  execute idempotency key, and test them separately from transport claims.
- Respect Telegram flood limits: answers edit no faster than the per-chat
  floor and split before the 4096-character message limit.

## Validation

From the repository root:

```bash
pnpm --filter telegrambot run check:types
pnpm --filter telegrambot test
```

The unit suite drives the real adapter against a fake Bot API and needs no
Telegram credential. When external validation is explicitly authorized and a
test bot is configured, verify one DM and one group `/ask@<bot>` produce one
execution and one answer each, and that an unlisted chat produces none.
