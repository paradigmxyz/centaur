# discordbot

Discord chat ingress for the Centaur agent. Mirrors `slackbotv2` (streamed, session-backed
replies to `@`-mentions) using Vercel's Chat SDK Discord adapter. The session logic is a
deliberate clone of `services/slackbotv2` kept in sync manually (there is no shared package);
the Rust `api-rs` control plane is unchanged (`discord:…` thread keys flow through identically).

## Behavior

- **`@`-mention in a channel** → the adapter creates a **public thread from that message**, the
  bot streams the answer inside it, and the thread is renamed to the message text. The session is
  keyed by the new thread (`discord:{guild}:{channel}:{threadId}`).
- **`@`-mention inside an existing thread** → the bot answers in that thread.
- **Follow-ups inside an active thread** append to the same session without a re-mention.
- **Append-only narration**: a run instantly reacts 👀 on the triggering message, then posts the
  agent's reasoning blurbs as their own *italic* messages as each thought completes (commands/tools
  are not rendered — they just end a thought). The **answer** streams into a separate message
  created when the first answer text arrives, so it lands at the bottom of the thread even when
  users chime in mid-run. On settle the 👀 flips to ✅ (or ❌); no bot message is ever edited or
  deleted.

## Ingress model

Discord delivers normal messages over a **Gateway WebSocket** (outbound), not HTTP webhooks. The
bot opens a single long-lived Gateway connection in "direct mode" (`startGatewayListener` with a
large duration; discord.js maintains the session with native RESUME). There is **no public
ingress** — only a `GET /health` endpoint that reflects the Gateway connection state.

> ⚠️ **Run exactly one replica.** Two pods on the same bot token open two Gateway sessions and
> every message is handled twice. Deploy with `replicas: 1` + `strategy: Recreate`, never autoscale.

> ⚠️ **Do not proxy the Gateway.** discord.js ignores `HTTPS_PROXY` for the WebSocket. Give the pod
> direct `:443` egress to Discord and exclude Discord hosts via `NO_PROXY`.

## Environment

| Var | Required | Notes |
|-----|----------|-------|
| `DISCORD_BOT_TOKEN` | ✅ | Bot token (account-level credential — keep secret). |
| `DISCORD_PUBLIC_KEY` | ✅ | Ed25519 public key (used by the adapter for any HTTP interactions). |
| `DISCORD_APPLICATION_ID` | ✅ | Doubles as the bot user id for mention detection. |
| `DISCORDBOT_GUILD_ALLOWLIST` | ✅ to do anything | Comma/space-separated guild IDs. **Fail-closed: empty ⇒ the bot ignores all messages.** |
| `DISCORDBOT_API_KEY` | – | Bearer to api-rs. Use a dedicated key, not the Slack one. |
| `CENTAUR_API_URL` | – | api-rs base URL (default `http://127.0.0.1:8080`). |
| `DISCORDBOT_DATABASE_URL` / `DATABASE_URL` / `POSTGRES_URL` | ✅ | Thread-state store. The bot refuses to boot without one (no silent localhost fallback). |
| `DISCORDBOT_TRIGGER_BOT_ALLOWLIST` | – | Comma/space-separated bot/webhook author IDs whose messages may enter sessions (e.g. a Sentry webhook). Empty (default) ⇒ all bot messages are ignored. Use the ID the message is authored as: the bot's user id, or the webhook id for webhook integrations. |
| `DISCORDBOT_MAX_CONCURRENT_EXECUTIONS_PER_GUILD` | – | In-flight execution cap per guild (default 3). Over the cap, the triggering message gets a 🚦 reaction and is kept as context only. |
| `DISCORDBOT_ACTIVE_EXECUTION_TTL_MS` | – | Staleness TTL for the per-thread active-execution flag (default 30 min) — unwedges threads after a crash mid-handoff. |
| `DISCORDBOT_ANSWER_EDIT_INTERVAL_MS` | – | Edit cadence for the streamed answer message (default 1500 ms, clamped to ≥1500 to respect Discord rate limits). |
| `DISCORD_MENTION_ROLE_IDS` | – | Role mentions that also trigger the bot. |
| `DISCORDBOT_NAME_THREADS` | – | Set `false` to keep the adapter's generic thread names. |
| `DISCORDBOT_USER_NAME` | – | Bot display name used for mention parsing/thread naming (default `centaur`; the chart sets it from `discordbot.userName`). |
| `DISCORDBOT_STATE_KEY_PREFIX` | – | Prefix for rows in the Postgres thread-state store (default `centaur-discordbot`). |
| `DISCORD_API_URL` | – | Override Discord API base. |
| `PORT` | – | Health server port (default 3001). |
| `SESSION_IDLE_TIMEOUT_MS` / `SESSION_MAX_DURATION_MS` | – | Forwarded to api-rs execute. |

DMs are denied by the guild allowlist: the adapter does request the DirectMessages intent, but a
DM has no guild, so the fail-closed allowlist check rejects it.

## Discord application setup

1. **Create the application** at <https://discord.com/developers/applications>. Note the
   **Application ID** and **Public Key** (General Information).
2. **Bot** tab → reveal/reset the **token** (`DISCORD_BOT_TOKEN`).
3. **Bot → Privileged Gateway Intents** → enable **Message Content Intent**. Without it,
   non-mention messages arrive with empty content and follow-ups break. (Bots in 100+ servers must
   apply for it; below that it's a toggle.)
4. **Invite the bot** (OAuth2 → URL Generator) with scope `bot` and permissions:
   _View Channels_, _Send Messages_, _Send Messages in Threads_, **Create Public Threads**,
   _Embed Links_, _Read Message History_, _Add Reactions_ (the 👀/✅ run-status indicator).
5. Set `DISCORDBOT_GUILD_ALLOWLIST` to the server(s) you invited it to — the bot is **inert** until
   this is set.

## Runtime assumptions (validated 2026-06-02)

A throwaway spike confirmed the three things the static build couldn't prove: discord.js's Gateway
runs under Bun, a Gateway `MESSAGE_CREATE` dispatches in-process to `chat.onNewMention`, and a
channel mention auto-creates a thread that the bot streams into. An `@`-mention produced a threaded
reply end-to-end. The spike has served its purpose and been removed.

## Develop / test

```bash
bun run check:types   # tsgo
bun test test         # allowlist, threading, gateway controller (no Discord needed)
bun run dev           # run the server locally (needs env above)
```

## Known limitations

- The Gateway listener can't expose the precise close code on a fatal end; an unexpected
  disconnect exits the process so Kubernetes restarts it (CrashLoopBackOff surfaces bad
  token/intents). `/health` liveness is "listener still running", not a deep socket probe.
- Concurrency is `'drop'`: the per-thread lock serializes handling so two near-simultaneous mentions
  can't double-execute. The tradeoff is that a follow-up sent *while a stream is still running* is
  dropped rather than appended mid-stream; send it again once the reply finishes.
- Thread renaming is best-effort, applies on the first execution, and only touches threads the
  bot created from a channel mention (`isThreadCreatedForMessage`); a mention inside a
  user-created thread never renames it (set `DISCORDBOT_NAME_THREADS=false` to disable renaming
  entirely).
- A Gateway RESUME that replays a channel mention before state commits could, in rare cases, let
  the adapter create a second thread (the dedup guards execution, but thread creation happens
  inside the adapter). See the plan's invariant #2.
