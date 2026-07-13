# githubbot

GitHub ingress for the Centaur agent. Mirrors `linearbot` (session-backed replies) in a
**comment-thread model**: a GitHub PR or issue comment thread maps to one centaur sandbox/context,
and the bot answers *in the thread* with a comment. It's built on the official
[`@chat-adapter/github`](https://www.npmjs.com/package/@chat-adapter/github) chat-SDK adapter, so
the session logic (`session-api.ts`) and rendering are the same as the other bots; the Rust `api-rs`
control plane is unchanged (`github:…` thread keys flow through identically).

The bot acts as a **real GitHub teammate**: it authenticates with a personal access token on a
dedicated machine-user account, so it can be `@`-mentioned, assigned, and **requested as a
reviewer** like any other collaborator.

## Behavior

- **`@`-mentioning the bot in an issue or PR comment** (Conversation tab) or a **PR review comment**
  (Files changed tab) → the bot answers in that thread, keyed `github:{owner}/{repo}:{prNumber}`
  (PR/issue level) or `github:{owner}/{repo}:{prNumber}:rc:{commentId}` (a review-comment thread) —
  one thread === one sandbox/context stack. For a **review-comment thread** the file path, line, and
  diff hunk it's anchored to are injected into the turn so the agent knows exactly what it's looking
  at; for a **PR conversation thread** the agent is pointed at `gh pr view`/`gh pr diff` to fetch the
  PR itself. A 👀 reaction acks the triggering comment while the bot works, settling to 🚀 / 😕. The
  reply is one comment: the answer with the chain-of-thought folded into a collapsed `<details>`
  section. Mention detection is the adapter's (matches the bot account's `@username`). Only authors
  whose GitHub `author_association` is allowed (default `OWNER` / `MEMBER` / `COLLABORATOR`) can drive
  a turn — the agent runs in a write-capable sandbox and posts its transcript back, so untrusted
  commenters can't steer it. Widen or open it with `GITHUBBOT_ALLOWED_AUTHOR_ASSOCIATIONS` (`*` allows
  everyone, e.g. a fully-private repo). Lifecycle triggers (assignment, review-request) are already
  gated by GitHub permissions, so this applies only to the comment path.
- **`@`-mentioning the bot in the body of a newly-opened issue or PR** (the description, not a
  comment) → the same conversational turn runs, keyed to that issue/PR thread, with the reply posted
  as a comment. Only the `opened` event is handled — an edit that adds a mention later won't
  re-trigger, so re-issue it as a comment. Same author gate as the comment path.
- **Plain comments in a thread the bot is already active in** (no mention) are appended to that
  thread's session as append-only context — no execution, no reply — so a follow-up like "actually,
  hold off" is seen by the next turn. The bot's own comments are skipped (loop guard) and inactive
  threads are ignored.
- **Requesting the bot's review on a PR** (`pull_request` / `review_requested` targeting the bot
  account — or a **team the bot belongs to**, whose membership is checked and briefly cached) → a
  review turn runs on a **dedicated, isolated session thread**
  (`github-review:{owner}/{repo}:{prNumber}`) — kept separate from the PR conversation so reviews
  never share a sandbox with chit-chat, but persistent per PR so a re-request builds on the prior
  review. The chat adapter only surfaces comment threads, so this lifecycle event is handled
  directly: githubbot verifies the webhook signature itself, and the agent reviews the PR in its
  sandbox, posting inline comments + a summary via `gh`. The **review methodology** is a bundled,
  standalone default (`src/review-prompt.ts`) — good and reliable with zero config — that a
  deployment can **fully replace** via `GITHUBBOT_REVIEW_PROMPT` / `GITHUBBOT_REVIEW_PROMPT_FILE`
  (the override is used verbatim, so org conventions supersede ours wholesale; for Splits this is
  where the overlay supplies its review guide). Webhook redeliveries are de-duplicated by delivery id.
- **Assigning an issue to the bot** (`issues` / `assigned` to the bot account) → an autonomous work
  turn runs on a **dedicated, isolated session thread** (`github-issue:{owner}/{repo}:{n}`): the agent
  reads the issue, implements a fix in its sandbox, and opens a PR (self-assigning it so it then
  manages that PR toward merge). Like reviews, this lifecycle event is handled directly (githubbot
  verifies the signature) and de-duplicated by delivery id. The **issue-work methodology** is a
  bundled, standalone default (`src/issue-prompt.ts`) that a deployment can **fully replace** via
  `GITHUBBOT_ISSUE_PROMPT` / `GITHUBBOT_ISSUE_PROMPT_FILE` (used verbatim, like the review prompt).
- **Per-turn context**: every turn prepends a compact header naming the PR/issue so a recycled
  sandbox always knows which subject to act on and where to reply.
- `--claude` / `--codex` / `--amp` / `--model …` / `--opus|--sonnet|--haiku` inline flags pick the
  harness/model, same as the other bots.

## PR self-management (v2)

For PRs the bot **owns** — i.e. **assigned to the bot account** — githubbot drives the PR toward merge
by reacting to lifecycle webhooks. Ownership is purely an assignment mechanism: assign a PR to the bot
to have it take over, and unassign to hand it back. It only ever acts on owned PRs, and on a dedicated
management thread (`github-manage:{owner}/{repo}:{n}`); the agent does its GitHub writes via `gh`.

- **Take over on assign.** Being assigned a PR is the explicit signal to take it over, so the bot
  immediately evaluates CI (fixing red or merging green) rather than waiting for the next lifecycle
  event.
- **Fix CI.** When **all** checks for a head SHA are settled (not per failing job — interwoven jobs
  make early firing harmful) and red, a fix turn diagnoses and pushes a fix. Bounded to
  `GITHUBBOT_CI_FIX_MAX_ATTEMPTS` consecutive attempts (default 3, reset when CI goes green); on
  exhaustion the bot comments tagging a human and stops. On the steady-state CI path it backs off if
  the failing head commit was authored by a human (it won't step on someone mid-edit) — except right
  after assignment, where being assigned is an explicit hand-off, so it fixes the PR regardless of who
  pushed last.
- **Address review.** A submitted review (`changes_requested` / `commented`) triggers one holistic
  turn that reads all the feedback, makes a single coherent commit, replies on each thread, resolves
  what it addressed, and re-requests review.
- **Merge when ready.** Deterministic — no agent. When GitHub reports the PR `mergeable_state == clean`
  the bot merges it (`GITHUBBOT_MERGE_METHOD`, default squash) and deletes the branch. `dirty` →
  conflict-resolution turn; `behind` → branch update; anything else → wait. Enabled by default for
  owned PRs; disable globally with `GITHUBBOT_AUTO_MERGE=false`, or per-PR with the hold label
  (`GITHUBBOT_HOLD_LABEL`, default `do-not-merge`) or by keeping the PR a draft.
- **Owned-PR conversation.** An @-mention in an owned PR's conversation (or a review-comment thread)
  runs in that PR's management session too — so the bot answers with the context of the CI fixes and
  review work it's been doing on the PR — while the rendered reply still posts to the comment thread.
  An @-mention in the conversation of an **issue assigned to the bot** likewise runs in that issue's
  work session (`github-issue:…`), so the bot replies with the context of the work it's doing on it.

> **Scope.** v2 targets **same-repo PRs on repos you control** (where you own the webhook). The
> fork → upstream contribution flow (e.g. PRs against `paradigmxyz/centaur`) is out of scope: it
> needs the upstream repo to deliver webhooks to this bot, which isn't yours to configure.
>
> **Op requirement:** the agent's sandbox `git`/`gh` identity must be able to push to the managed
> PR branches (ideally authenticated as the bot account, so commits and replies come from it).

## Ingress model

GitHub delivers **HTTP webhooks** to `POST /api/webhooks/github` (content type **must** be
`application/json`). Comment events (`issue_comment`, `pull_request_review_comment`) are handed to the
chat adapter, which verifies the `X-Hub-Signature-256` HMAC and maps them to thread/message events.
Lifecycle events (`pull_request`, `pull_request_review`, `issues`, and the CI events) are handled by
githubbot directly (the adapter ignores them), so githubbot verifies the signature itself before acting. Turns run in the background — webhooks are acknowledged
immediately (cold sandbox spin-up far exceeds GitHub's webhook deadline), with a bounded retry inside
the turn for transient cold-start failures. On `SIGTERM` (a deploy/rollout) the bot stops accepting
webhooks and **drains in-flight turns** for up to `GITHUBBOT_SHUTDOWN_DRAIN_MS` before exiting, so
running work isn't dropped (claims are taken before the work, so a dropped turn would never retry).
It also **serializes turns targeting the same session** so two turns can't interleave git/push in one
sandbox. Both assume the **single replica** the chart runs (`replicaCount: 1`).

## Auth

A personal access token for the bot's GitHub teammate account is required (`GITHUB_TOKEN`). As a
normal user account it is natively mentionable, assignable, and requestable as a reviewer, and the
token inherits that user's permissions. Scopes: **`repo`** (read PRs/issues, post and edit comments,
add reactions) — and, when the agent pushes branches or opens PRs from its sandbox, **`workflow`**.

Keep this distinct from the `GITHUB_TOKEN` used by the repo-cache / sandbox tooling — that one is the
agent's git-operations token; this one is the bot's own identity. The chart wires githubbot's token
from a separate `GITHUBBOT_TOKEN` secret key to avoid collision.

GitHub App auth is also supported by the adapter (`GITHUB_APP_ID` / `GITHUB_PRIVATE_KEY`), but the
PAT-teammate model is what we run.

Webhook events to subscribe: **Issue comments**, **Pull request review comments**, **Issues**, **Pull
requests**, **Pull request reviews**, **Check runs**, **Check suites**, and **Workflow runs**
(**Issues** drives issue-work-on-assignment; the last four drive v2 PR self-management).

## Environment

| Var | Required | Notes |
|-----|----------|-------|
| `GITHUB_TOKEN` | ✅ | PAT for the bot's teammate account. |
| `GITHUB_WEBHOOK_SECRET` | ✅ | Webhook signing secret (or `GITHUBBOT_WEBHOOK_SECRET`). |
| `GITHUB_BOT_USERNAME` | ✅ | The bot account's GitHub login — drives `@`-mention and requested-reviewer matching (or `GITHUBBOT_USER_NAME`). |
| `GITHUBBOT_DATABASE_URL` | ✅ | Postgres for chat-SDK state (falls back to `DATABASE_URL` / `POSTGRES_URL`). |
| `CENTAUR_API_URL` | — | api-rs control plane, default `http://127.0.0.1:8080`. |
| `GITHUBBOT_API_KEY` | — | Bearer sent to api-rs (falls back to `CENTAUR_API_KEY`). |
| `GITHUBBOT_DEFAULT_HARNESS` | — | Harness for new threads without an inline flag, default `codex`. |
| `GITHUBBOT_REVIEW_PROMPT` | — | Full review methodology, inline. Replaces the bundled default verbatim. |
| `GITHUBBOT_REVIEW_PROMPT_FILE` | — | Path to a file holding the review methodology (e.g. an overlay-mounted file). Used when the inline var is unset. |
| `GITHUBBOT_ISSUE_PROMPT` | — | Full issue-work methodology, inline. Replaces the bundled default verbatim. |
| `GITHUBBOT_ISSUE_PROMPT_FILE` | — | Path to a file holding the issue-work methodology (e.g. an overlay-mounted file). Used when the inline var is unset. |
| `GITHUBBOT_MANAGEMENT_PROMPT` | — | Extra guidance prepended to owned-PR management turns (CI-fix / conflict / address-review), inline. The per-action preamble still rides underneath. |
| `GITHUBBOT_MANAGEMENT_PROMPT_FILE` | — | Path to a file holding the management guidance (e.g. an overlay-mounted file). Used when the inline var is unset. |
| `GITHUBBOT_ALLOWED_AUTHOR_ASSOCIATIONS` | — | Comma-separated `author_association` values allowed to drive the comment path. Default `OWNER,MEMBER,COLLABORATOR`; `*` allows everyone. |
| `GITHUB_API_URL` | — | Override the GitHub REST base URL (GitHub Enterprise). |
| `GITHUBBOT_USER_ID` | — | Bot's numeric user id for self-message detection (auto-detected otherwise). |
| `GITHUBBOT_STATE_KEY_PREFIX` | — | Chat-SDK state key prefix, default `centaur-githubbot`. |
| `GITHUBBOT_LOG_LEVEL` | — | `debug`/`info`/`warn`/`error`, default `info`. |
| `GITHUBBOT_AUTO_MERGE` | — | Auto-merge owned PRs when mergeable. Default `true`. |
| `GITHUBBOT_MERGE_METHOD` | — | `merge` / `squash` / `rebase`. Default `squash`. |
| `GITHUBBOT_HOLD_LABEL` | — | Label that pauses auto-merge. Default `do-not-merge`. |
| `GITHUBBOT_CI_FIX_MAX_ATTEMPTS` | — | Consecutive CI-fix attempts before escalating. Default 3. |
| `GITHUBBOT_DELETE_BRANCH_ON_MERGE` | — | Delete head branch after merge. Default `true`. |
| `GITHUBBOT_ESCALATION_HANDLE` | — | Fallback @handle (no leading @) tagged when the bot gives up. |
| `SESSION_IDLE_TIMEOUT_MS` / `SESSION_MAX_DURATION_MS` | — | Forwarded to api-rs executes. |
| `GITHUBBOT_SHUTDOWN_DRAIN_MS` | — | How long to let in-flight turns finish on `SIGTERM` before exiting. Default `25000`; the chart derives it from the pod's termination grace period. |

## Tests

`bun test test` — unit tests for the override flag parser, the GitHub thread-key parsing / context
preamble, the review-request trigger gating (incl. team requests), the issue-assignment gating, the
v2 PR-manager decision logic (CI evaluation, assignment-based ownership, merge gating, the CI-fix
counter / escalation, and the merge-claim release-on-failure), the author-association gate, body
mentions, and the per-session serialization queue.
