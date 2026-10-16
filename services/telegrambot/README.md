# telegrambot

Telegram chat ingress for the Centaur agent, built on the Vercel Chat SDK with the official
`@chat-adapter/telegram` adapter and `@chat-adapter/state-pg`. Session forwarding, render
obligations, and recovery mirror `services/discordbot`; api-rs derives Telegram principals from
the adapter's `telegram:…` thread keys.

## Behavior

- **Private chat with an allowlisted user** → every message starts a turn.
- **Allowlisted group or supergroup** → a turn starts on a **reply to one of the bot's messages**
  or on **`/ask@<bot> <question>`**. Sending `/ask@<bot>` as a reply to someone else's message
  quotes that message into the question.
- Plain `@<bot>` mentions and unaddressed commands are ignored in groups (see
  [Triggers](#triggers)), unless [group observation](#group-observation) is on.
- **Run status**: the triggering message gets 👀, then 👍 on success or 👎 on failure (Telegram only
  allows a fixed reaction set). A typing indicator runs while the agent works. No progress
  messages are posted.
- **Answers** stream into a new message (a reply to the trigger in groups), edited no faster than
  Telegram's flood limits and split across messages before the 4096-character limit.
- **Sessions**: one durable session per chat, or per forum topic
  (`telegram:<chat_id>[:<topic_id>]`). A trigger that arrives while a run is active joins the
  session as context and gets ✍; it does not start a second run.
- `/help`, `/start`, and other commands addressed to the bot reply with usage text.

## Triggers

With privacy mode on (BotFather's default), Telegram delivers replies to the bot's messages,
commands, and DMs, but does not reliably deliver messages that only `@mention` the bot. Treating
a mention as a trigger would therefore work only sometimes, and with privacy mode off it would
make every group message a candidate trigger. Groups therefore use replies and the addressed
`/ask@<bot>` command, which the adapter routes as a slash command. A bare `/ask` in a group may be
meant for another bot and is ignored.

## Group observation

`TELEGRAMBOT_OBSERVE_GROUPS=true` (`telegrambot.observeGroups`) gives groups the Slack bot's
behavior. It expects privacy mode **off** in BotFather (`/setprivacy` → Disable; re-add the bot to
existing groups afterwards), so Telegram delivers every group message.

- A plain `@<bot>` mention from an allowlisted user also starts a turn.
- Every message in an allowlisted group, from any member, is kept (up to 200 per chat, 7 days)
  through the Chat SDK thread-history cache. A turn started in the group carries the kept
  messages since the bot was last addressed (newest 50, text only;
  `TELEGRAMBOT_GROUP_CONTEXT_MAX_MESSAGES`), the way the Slack bot carries a thread's earlier
  replies.
- Nothing from other chats, or from DMs of non-allowlisted users, is ever stored. The adapter's
  own history persistence is turned off in every mode, because it stores each message before the
  allowlist runs.
- In polling mode a message that arrives while the chat is busy is retried until it is kept. In
  webhook mode it is acknowledged first, so such a message can be missing from the context.

## Company context

`TELEGRAMBOT_CONTEXT_CHAT_ALLOWLIST` (`telegrambot.contextChatAllowlist`) connects groups to
company context. Telegram has no history API and allows one update consumer per bot token, so
this service writes every message from a connected group, from any member, to the
company-context service's `company_context_system.telegram_messages` table as it arrives. That
service (`experimentalCompanyContext.indexers.telegram`) projects the messages into chat-day
documents that every principal can search through its query API, and expires them after its
retention window.

- Connected groups are company-wide. Do not connect a group whose messages should stay private.
- Agents read the messages with `company_context v2 search --type telegram_message` and
  `company_context v2 read`. The query API shows sessions from Telegram principals only Telegram
  documents. Do not grant Telegram principals, or roles they hold, the `company_context` tool's
  `company_context_dsn` Postgres secret: the legacy index it opens is not scoped for Telegram.
- The list is independent of `TELEGRAMBOT_CHAT_ALLOWLIST`: a connected group's members still
  need the answer allowlists to use the bot, and the bot can answer in a group without
  connecting it.
- Privacy mode must be **off** for the bot to see unaddressed messages (as for group
  observation).
- Text, captions, sender, reply, and topic ids are stored, plus each attachment's kind, file
  name, and MIME type; file contents are not. Private chats, edits, and the bot's own messages are
  never stored. Telegram does not tell bots about deletions, so a deleted message stays until it
  expires.
- Albums are stored as one row (the last item's id) carrying the caption and every item. Service
  messages with no text or media, such as joins, are skipped.
- Storing is best effort: writes are not awaited and time out after 5 seconds, so they never delay
  an answer; a failed write (for example, before the company-context service has migrated the
  table) is logged as `telegrambot_context_record_failed`.
- Writes go through `TELEGRAMBOT_DATABASE_URL`, which must therefore reach the company-context
  database with a role that can insert into the table. The chart gives both services the same
  database, and refuses a context list unless `experimentalCompanyContext.indexers.telegram` is enabled.
- Upgrading a group to a supergroup changes its chat id; update the list to keep indexing it.

## Access policy

Telegram bots are reachable by anyone who knows the username, and there is no workspace or guild
boundary. Access is fail-closed:

- `TELEGRAMBOT_USER_ALLOWLIST` lists the user ids allowed to DM the bot.
- `TELEGRAMBOT_CHAT_ALLOWLIST` lists the group/supergroup chat ids where it answers. In a group,
  the sender must also be on the user allowlist: membership spreads through invite links, so
  allowlisting a chat does not vouch for everyone in it. A user allowlist alone never opens a group.
- Both empty ⇒ the bot is inert.
- Always denied: other bots, inline-bot relays (`via_bot`), messages sent as a channel or by an
  anonymous admin (`sender_chat`), linked-channel auto-forwards, edits, and channel posts.

Polling requests only `message` updates, so edits, channel posts, callbacks, and reactions are
never fetched.

## Ingress modes

**Polling (default).** The adapter long-polls `getUpdates`, deletes any registered webhook at
startup, and keeps a durable checkpoint in Postgres. A handler that fails with a transient
api-rs error is retried in-process, then re-thrown so the checkpoint redelivers the update with
backoff. No public endpoint is needed.

**Webhook (opt-in).** Set `TELEGRAMBOT_MODE=webhook` and `TELEGRAM_WEBHOOK_SECRET_TOKEN`. The
service then serves `POST /api/webhooks/telegram`; the adapter rejects requests without the
matching `X-Telegram-Bot-Api-Secret-Token` header (401) and claims each `update_id` once.
Register the webhook yourself with the same secret and `allowed_updates: ["message"]`. Webhook
handling is acknowledged before the handler runs, so a handoff failure after the in-process
retries is logged and not redelivered.

> ⚠️ **Run exactly one replica** (`replicas: 1`, `strategy: Recreate`). Telegram rejects
> concurrent `getUpdates` consumers for one token.

`GET /health` returns 503 in polling mode when the polling loop is not running. The process exits
at startup if `getMe` fails (bad token or blocked egress), so misconfiguration crash-loops
visibly.

## Environment

| Var | Required | Notes |
|-----|----------|-------|
| `TELEGRAM_BOT_TOKEN` | ✅ | Bot token from @BotFather. Embedded in Bot API URL paths; never log request URLs. |
| `TELEGRAMBOT_USER_ALLOWLIST` | for DMs | Comma/space-separated Telegram user ids. **Empty ⇒ no DMs and no group senders.** Also gates who may trigger the bot in groups. |
| `TELEGRAMBOT_CHAT_ALLOWLIST` | for groups | Comma/space-separated group/supergroup chat ids (negative numbers). **Empty ⇒ no groups.** |
| `TELEGRAMBOT_CONTEXT_CHAT_ALLOWLIST` | – | Group/supergroup chat ids connected to company context (see [Company context](#company-context)). **Empty ⇒ nothing stored.** |
| `TELEGRAMBOT_DATABASE_URL` / `DATABASE_URL` / `POSTGRES_URL` | ✅ | Chat SDK state (polling checkpoint, dedupe, locks, render obligations). Boot fails without one. |
| `TELEGRAMBOT_API_KEY` | – | Bearer to api-rs; api-rs scopes it to `telegram:` sessions. |
| `CENTAUR_API_URL` | – | api-rs base URL (default `http://127.0.0.1:8080`). |
| `TELEGRAMBOT_MODE` | – | `polling` (default) or `webhook`. |
| `TELEGRAM_WEBHOOK_SECRET_TOKEN` | webhook | Required in webhook mode. |
| `TELEGRAM_BOT_USERNAME` | – | Bot username; resolved from `getMe` when unset. |
| `TELEGRAM_API_BASE_URL` | – | Bot API base override (self-hosted Bot API server). |
| `TELEGRAMBOT_POLL_TIMEOUT_SECONDS` | – | `getUpdates` long-poll timeout (adapter default 30). |
| `TELEGRAMBOT_ANSWER_EDIT_INTERVAL_MS` | – | Answer edit floor (default 1500 in DMs, 3100 in groups; minimum 1000). |
| `TELEGRAMBOT_ACTIVE_EXECUTION_TTL_MS` | – | Staleness TTL for a chat's active-run flag (default 30 min). |
| `TELEGRAMBOT_STATE_KEY_PREFIX` | – | Postgres state key prefix (default `centaur-telegrambot`). |
| `PORT` | – | HTTP port (default 3001). |
| `SESSION_IDLE_TIMEOUT_MS` / `SESSION_MAX_DURATION_MS` | – | Forwarded to api-rs execute. |

## Telegram setup

1. Create a bot with [@BotFather](https://t.me/BotFather) (`/newbot`) and store the token as
   `TELEGRAM_BOT_TOKEN`.
2. Leave **privacy mode enabled** (the default). The trigger rules above do not need more.
3. Optionally register commands with BotFather (`/setcommands`: `ask - Ask the agent`).
4. Add the bot to the group. Put the group's chat id in `TELEGRAMBOT_CHAT_ALLOWLIST` and the id of
   every user who may use the bot (in DMs or the group) in `TELEGRAMBOT_USER_ALLOWLIST`. Ignored messages are logged with their `chat_id`
   (`telegrambot_message_ignored_chat_not_allowlisted`), which is one way to find an id.
5. For reactions in groups, the group must allow the 👀/👍/👎/✍ reactions (the default).

## Develop / test

```bash
bun run check:types
bun test test   # real adapter against a fake Bot API and a fake session API
# Optional: the company-context staging-table contract test needs a ParadeDB superuser URL.
TELEGRAMBOT_CONTEXT_TEST_DATABASE_URL=postgresql://postgres:postgres@localhost:5432/postgres bun test test
bun run dev     # needs the environment above
```
