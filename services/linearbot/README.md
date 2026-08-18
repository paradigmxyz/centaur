# linearbot

Linear agent ingress for the Centaur agent. Mirrors `slackbotv2` (session-backed replies) in a
**comment-thread model**: a Linear comment thread maps to one centaur sandbox/context, and the bot
answers *in the thread* with a single, live-edited comment. The session logic is a deliberate clone
of `services/slackbotv2` kept in sync manually (there is no shared package); the Rust `api-rs`
control plane is unchanged (`linear:…` thread keys flow through identically).

> Linear's native **agent sessions** are deliberately **off**: making the app an agent turns an
> `@`-mention into a session widget that *consumes* the comment, destroying the interactive comment
> thread the product wants. Work is therefore triggered off plain `Comment` and `Issue` webhooks,
> not `AgentSessionEvent`s. If an agent session ever does open, it is **vestigial** — acked and
> settled with a one-line "I'll reply in the comment thread" pointer; the real answer is always the
> comment.

## Behavior

- **`@`-mentioning the bot in a comment** → the bot answers in that comment thread, keyed
  `linear:{issueId}:c:{rootCommentId}` (one thread === one sandbox/context stack). The reply is a
  single comment, live-edited: it posts with the latest reasoning line as a headline above a
  collapsed **Thinking…** section that fills in as the run streams (throttled), then swaps in place
  to the final answer above a collapsed **Chain of thought** section. A 👀 reaction acks the
  triggering comment while the bot works, settling to ✅ / ❌. A mention is encoded by Linear as the
  bot profile's plain URL in the markdown body, so detection matches that (with the user id and a
  typed `@name` as fallbacks).
- **Plain comments in a thread the bot is already active in** (no mention) are appended to that
  thread's session as append-only context — no execution, no reply — so a follow-up like "actually,
  hold off" is seen by the next turn. The bot's own comments are skipped (loop guard) and inactive
  threads are ignored (an issue can host many unrelated threads). Requires the **Comments** webhook
  subscription.
- **Assigning or delegating an issue to the bot** → an assignment turn runs on the issue-level
  thread (`linear:{issueId}`) and posts its result as a comment. Driven by the **Issue** webhook
  (`create`, or `update` gated on the assignee/delegate field actually changing in `updatedFrom`, so
  unrelated edits — labels, descriptions, the bot's own status write bouncing back — don't re-run
  it). The turn posts an "On it" comment immediately (no triggering comment to react to) and runs a
  synthesized "work this issue to the best of your ability" instruction (a bare handoff carries no
  user prompt).
- **Owned issues track progress via workflow status** (mentions never move status — the agent owns
  only issues *assigned/delegated* to it): kicking off an assignment turn moves Todo/Backlog/Triage
  → the team's first started state; at the end the agent either moves the issue itself with the
  `linear` tool or ends its answer with `Linear-Status: done|in_progress|todo`, which is stripped
  from the posted comment and applied by the bot. Status is written **only** on the assignment turn
  (the issue-level thread is the sole status owner); comment turns never write it — so a commenter
  can't force a transition via the marker, and a delegate-plus-mention can't race two threads onto
  the same issue. Best-effort.
- **Ownership contract**: when the issue is assigned or delegated to the bot — on the assignment
  turn AND on comment turns where the bot is the delegate — an ownership note is injected so the
  agent carries the work forward (and knows how to signal status), not just answers, plus the
  recurring-task continuity hint and a no-self-delegation rule.
- **Issue context first**: each turn fetches the issue (identifier, title, state, url, description,
  delegate) and prepends it inline to the execute — full context on the thread's first turn, a
  compact id/title header thereafter — so a recycled sandbox always knows what the task is.
  (`Comment`/`Issue` webhooks carry no `promptContext` blob, unlike agent-session events, so the bot
  fetches the issue itself.)
- `--claude` / `--codex` / `--amp` / `--model …` / `--opus|--sonnet|--haiku` inline flags pick the
  harness/model, same as slackbotv2.

## Ingress model

Linear delivers **HTTP webhooks** (like Slack, unlike Discord's Gateway): signature-verified
(`LINEARBOT_WEBHOOK_SECRET`, HMAC-SHA256) deliveries to `POST /api/webhooks/linear`. For payloads
that carry user input (`Comment`/`create`, `Issue`/`create|update`, and any vestigial
`AgentSessionEvent`) the create/append handoff is awaited before the webhook is acknowledged, so a
retryable session-api failure answers **503** and Linear redelivers. The execute call runs inside
the background render — after the working ack — because cold sandbox spin-up far exceeds Linear's
webhook deadline. Multiple replicas are fine.

## Auth

A Linear token is required — set exactly one of two paths. Either way the token's identity must be
one you can `@`-mention and assign/delegate issues to, and it does the same work: read issues, post
and live-edit comments, add reactions, and move workflow status on owned issues.

- **Personal `LINEAR_API_KEY`** (simplest) — runs the comment-thread model as a regular Linear user,
  no OAuth install. There are no scopes to configure: a normal user is natively mentionable and
  assignable, and the key inherits that user's read/write permissions.
- **OAuth `actor=app` install** (`LINEAR_ACCESS_TOKEN`) — runs the bot as an app. Install with the
  `actor=app` authorization parameter (requires workspace admin) and request these scopes:

  | Scope | Why it's needed |
  |-------|-----------------|
  | `read` | always present; reads issues, comments, reactions, the bot's own `viewer`, and team workflow states |
  | `write` | live-edits comments, adds reactions, and moves issue status — none of these are covered by the granular `comments:create` / `issues:create` scopes, so broad `write` is required |
  | `app:mentionable` | puts the bot in the `@`-mention autocomplete — without it the bot can't be mentioned at all, and mentioning is the primary trigger |
  | `app:assignable` | lets an issue be assigned/delegated to the bot (it becomes the issue *delegate*, not assignee) — the ownership/assignment trigger |

Webhook subscriptions needed: **Comments** and **Issues**. Do **not** subscribe to **Agent session
events** — `app:mentionable` / `app:assignable` make Linear auto-create a native agent session on
every mention/delegate (this can't be turned off), but we never subscribe to those events and drive
work off the plain Comment/Issue webhooks instead; any session that does open is settled as
vestigial (see above). The `app:` scopes are required regardless — they are what make the bot
mentionable and assignable in the first place, not an agent-session add-on.

## Environment

| Var | Required | Notes |
|-----|----------|-------|
| `LINEARBOT_WEBHOOK_SECRET` | ✅ | Signing secret from the linearbot webhook's settings page. Distinct from the api-rs `linear_webhook` workflow's `LINEAR_WEBHOOK_SECRET` (separate Linear webhook → separate secret). |
| `LINEAR_ACCESS_TOKEN` | ✅* | actor=app OAuth token (*or `LINEAR_API_KEY`). |
| `LINEARBOT_DATABASE_URL` | ✅ | Postgres for chat-SDK state (falls back to `DATABASE_URL`). |
| `CENTAUR_API_URL` | — | api-rs control plane, default `http://127.0.0.1:8080`. |
| `LINEARBOT_API_KEY` | — | Dedicated bearer sent to api-rs. |
| `LINEARBOT_USER_NAME` | — | Bot display name for mention parsing, default `centaur` (the bot also derives its real handle/name from its own token). |
| `LINEARBOT_LOG_LEVEL` | — | `debug`/`info`/`warn`/`error`, default `info`. |
| `SESSION_IDLE_TIMEOUT_MS` / `SESSION_MAX_DURATION_MS` | — | Forwarded to api-rs executes. |

## Patched adapter

`patches/@chat-adapter__linear@4.31.0.patch` (registered in `pnpm-workspace.yaml`) carries three
fixes to the agent-session path. They are still applied but matter only if agent sessions are ever
enabled (they are off today, see above):

1. Agent-session messages encode their thread id as `linear:{issueId}:s:{agentSessionId}`
   regardless of which comment triggered them, so one Linear agent session maps to exactly one
   centaur session.
2. A `created` event without a root comment (description mention, bare delegation) synthesizes an
   empty trigger comment instead of dropping the event (which left the session unacknowledged
   forever).
3. A `created` event without a creator (automation/triage-rule delegation, or another agent) is
   attributed to a distinct `linear-automation` identity instead of the bot itself — upstream's
   self-attribution made the chat SDK skip the message as the bot's own, silently ignoring every
   automation-created session.

## Tests

`bun test test` — unit tests plus an emulate-style harness (fake Linear GraphQL API + mock api-rs +
signed webhooks) that drives the real patched adapter end-to-end.
