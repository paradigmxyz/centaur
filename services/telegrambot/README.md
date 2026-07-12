# telegrambot

Telegram chat ingress for the Centaur agent. Mirrors `discordbot`'s pull model (single-replica
service that fetches its own events instead of receiving webhooks) with a **long-polling
`getUpdates`** transport. There is no Chat SDK Telegram adapter, so the transport, durable inbox,
and rendering are bespoke; the session logic is a deliberate clone of `services/discordbot` kept in
sync manually. The Rust `api-rs` control plane derives Telegram principals from the typed
`telegram:…` thread keys.

## Behavior

- **Reply to the bot or send `/ask@botname <text>` in an allowlisted group** → the bot reacts 👀,
  posts throttled progress blurbs, and streams the answer into the chat (edited in place, split
  into ≤4096-char messages when long). On settle 👀 flips to ✅ (or ❌).
- **Message the bot in an allowlisted DM** → every message triggers or appends, no addressing
  needed.
- **Follow-ups in the same chat/topic** append to the same durable session
  (`telegram:chat:{chatId}[:{topicId}]` / `telegram:private:{chatId}[:{topicId}]`); a follow-up
  during an active execution is steered into it via api-rs instead of starting a second run.
- Plain textual `@botname` mentions are **not** a guaranteed trigger with privacy mode enabled;
  see the acceptance-test note in `AGENTS.md`.

## Ingress model

The bot long-polls `getUpdates` (one consumer per token; Telegram rejects concurrent pollers) and
durably persists every returned update batch plus the receipt cursor in one Postgres transaction
before dispatching work. Processing progresses through an idempotent stage machine
(`received → message_appended → execution_accepted → render_obligation_persisted → terminal`), so a
crash at any handoff resumes without duplicating executions. Terminal answer delivery is
**at-least-once**: a send whose response was never recorded is retried after restart and may
duplicate the final message, but never re-executes the agent.

> ⚠️ **Run exactly one replica** (`replicas: 1` + `strategy: Recreate`). Correctness does not rely
> on that: a fenced Postgres lease (holder + generation) guards the cursor, inbox claims, and
> render recovery, so a stale pod cannot advance state after losing ownership.

There is no public ingress — only health endpoints: `GET /live` (process liveness only) and
`GET /ready` (database + schema + ownership + webhook reconciliation + fresh poll progress).

## Environment

| Var | Required | Notes |
|-----|----------|-------|
| `TELEGRAM_BOT_TOKEN` | ✅ | Bot token from @BotFather. Embedded in every Bot API URL path — never log request URLs unredacted. |
| `TELEGRAMBOT_CHAT_ALLOWLIST` | ✅ for groups | Comma/space-separated chat ids allowed for group/supergroup chats. **Fail-closed: empty ⇒ no group work.** |
| `TELEGRAMBOT_USER_ALLOWLIST` | ✅ for DMs | Comma/space-separated Telegram user ids allowed for private chats. **Fail-closed: empty ⇒ no DMs.** There is no workspace boundary on Telegram; the allowlists carry the entire access-control load. |
| `TELEGRAMBOT_API_KEY` | – | Bearer to api-rs. Use a dedicated key. |
| `CENTAUR_API_URL` | – | api-rs base URL (default `http://127.0.0.1:8080`). |
| `TELEGRAMBOT_DATABASE_URL` / `DATABASE_URL` / `POSTGRES_URL` | ✅ | Durable inbox + render state. The bot refuses to boot without one (no silent localhost fallback). |
| `TELEGRAMBOT_USER_NAME` | – | Bot display name for docs/messages (default `centaur`). |
| `TELEGRAMBOT_POLL_TIMEOUT_S` | – | getUpdates long-poll timeout (default 50). |
| `TELEGRAMBOT_LEASE_TTL_MS` | – | Fenced-ownership lease TTL (default 30000). |
| `TELEGRAMBOT_MAX_CONCURRENT_THREADS` | – | Cross-thread worker concurrency (default 4); updates within one thread stay FIFO. |
| `TELEGRAMBOT_ANSWER_EDIT_INTERVAL_MS` | – | Edit cadence for the streamed answer message (default 1500 ms). |
| `TELEGRAMBOT_ANSWER_MAX_MESSAGES` | – | Max Telegram messages per answer; longer answers end with an honest truncation notice (default 8). |
| `TELEGRAMBOT_RETENTION_HOURS` | – | Terminal inbox rows below the receipt cursor are pruned after this age (default 72). |
| `TELEGRAM_API_URL` | – | Override the Bot API base (default `https://api.telegram.org`), used by tests/emulation. |
| `PORT` | – | Health server port (default 3001). |
| `SESSION_IDLE_TIMEOUT_MS` / `SESSION_MAX_DURATION_MS` | – | Forwarded to api-rs execute. |

## Platform setup

1. Create a bot with [@BotFather](https://t.me/BotFather) (`/newbot`) and note the token.
2. Leave **privacy mode enabled** (the default). The bot then receives replies to its messages,
   `/commands`, and DMs — exactly the v1 trigger surface. Disabling privacy mode or making the bot
   a group admin delivers *all* group traffic; the local trigger + allowlist gates still discard
   what should not run, but prefer minimal delivery.
3. Add the bot to the target group, then put that chat id in `TELEGRAMBOT_CHAT_ALLOWLIST` (send a
   message in the group and read `chat.id` from the bot logs, or use @userinfobot).
4. For DMs, put each user's Telegram id in `TELEGRAMBOT_USER_ALLOWLIST`.
5. Do **not** configure a webhook for this token: `getUpdates` and webhooks are mutually exclusive.
   The service calls `deleteWebhook` (preserving pending updates) at startup.
