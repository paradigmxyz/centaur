# RFC 0006: Deferred Action Approval

Status: Sketch
Owner: TBD
Target: `services/api-rs`, `services/slackbotv2`, `services/workflow-python`, tool SDK

## Summary

Let the agent queue side-effecting tool calls instead of performing them, keep
working, and hand the human one approval card when the turn ends. Approved
actions are applied by the control plane, outside the sandbox, with the
credential the sandbox never held. Rejected actions never reach the provider.

The enforcement comes from a primitive Centaur already has: iron-proxy secret
rules scoped by host, method, and path. During a turn, a gated integration's
credential covers its read endpoints only. The write endpoints get the
credential only when an approved action is applied.

The idea is borrowed from Cloudflare OS's gatekeepers
([cloudflare/cloudflare-os](https://github.com/cloudflare/cloudflare-os),
`packages/gatekeeper-kit`), adapted to a design where tool code runs inside
the sandbox.

## Motivation

Today every human-in-the-loop check is a sentence in a system prompt: "confirm
before sending." Three things go wrong with that.

**It costs a turn per confirmation.** The agent stops, asks, and the
requester's "yes" starts a new execution. On one deployment, turns run
160 to 360 seconds with 7 to 18 serial tool rounds, so a skill that writes 5
CRM records behind 5 confirmations costs the requester most of an hour of
babysitting. People respond by pre-authorizing broadly, which is the outcome
the confirmation existed to prevent.

**Nothing enforces it.** The harnesses run without interactive approval
(`AskForApproval::Never` for Codex in `crates/harness-server`). A prompt
injection that convinces the agent the user already said yes meets no second
check, because the sandbox can already reach the write endpoint with a working
credential.

**The approval text is the agent's prose.** The approver reads the agent's
summary of what it will send. Text from a source document can shape that
summary, and nothing ties the approved text to the bytes that go out. One
deployment prototyped a fix at the prompt level, in its overlay: a script that
renders the approval message from a payload with fixed titles, literal fields,
and a payload hash checked before sending. It works only as well as the agent
follows instructions.

## Goals

- A gated write cannot reach the provider without an approval recorded
  outside the sandbox. Bypassing the SDK (raw `curl` with the placeholder)
  fails at the proxy, not at the agent's discretion.
- One approval card per turn, posted when the execution completes, instead
  of one turn per confirmation.
- The approver sees the exact values that will be sent, rendered by code from
  the stored payload. What is applied is byte-for-byte what was shown.
- Approval identity comes from Slack's `block_actions` payload (a user ID
  Slack vouches for), never from message text.
- Integrations opt in one at a time. Ungated integrations behave as today.

## Non-Goals

- Simulated reads. Cloudflare OS projects pending writes onto later reads so
  the agent can read its own queued changes. v1 does not; see Phase 3.
- Payments, trades, and transfers. These stay `await-decision` or are never
  exposed at all. Batching makes it easier to approve something by accident,
  and a payment is the wrong place to take that risk.
- A cross-thread approval inbox. The card lives in the thread that queued it.
- Gating GraphQL-only or single-endpoint providers. Path rules can't separate
  their reads from their writes; see Open Questions.
- Replacing the system prompt's judgment about *what* to propose. This RFC
  governs whether a proposed write happens, not which writes are proposed.

## Current State

- Tool code runs inside the sandbox. Credentials never enter it: tools send
  placeholders, and the per-sandbox iron-proxy sidecar swaps in the real
  value (`swapped` in the audit log) or injects a header (`injected`).
- iron-proxy `secrets` rules already match on `host`, `methods`, and `paths`
  (globs). Centaur renders them per principal through `POST
  /api/v1/proxy/sync`, and a config barrier applies the rendered config
  before each execution's input runs.
- iron-proxy's `judge` transform can reject a matching request after an LLM
  call. It cannot hold a request or return a stand-in response.
- slackbotv2 already dispatches Block Kit clicks to durable workflows.
  `ctx.slack_buttons()` in `services/workflow-python` posts buttons whose
  `action_id` carries the `centaur.workflow.action:` prefix. Clicks are
  deduplicated by Slack click identity, and non-workflow clicks are emitted as
  `slack.block_action.<action_id>` workflow events.
- Python workflows can `ctx.wait_for_event()` (backed by absurd's
  `await_event`), and api-rs exposes `emit_workflow_event`.
- Closed PR #1211 proposed sandbox permission requests: the agent asks an
  admin for a new *grant*. This RFC covers a different question: whether one
  specific *action*, under grants the session already has, should happen.
- api-rs emits `session.execution_completed` per execution, and executions
  are serialized per thread.
- RFC 0005 (draft) adds a per-turn requester principal, which identifies the
  person allowed to approve a channel turn's actions.

## Design

### 1. Declaring an action (tool SDK)

A tool method with an externally visible side effect declares itself:

```python
@action(
    kind="gmail.send",
    delivery="queue",           # or "await-decision"
    describe=lambda p: Description(
        title="Send an email",  # fixed per kind, never agent text
        fields=[
            Field.inline("To", p["to"]),
            Field.inline("Subject", p["subject"]),
            Field.verbatim("Body", p["body"]),
        ],
    ),
)
def send(self, to: str, subject: str, body: str) -> dict: ...
```

Calling a declared method inside a turn does not call the provider. The SDK
posts `{tool, method, kind, payload}` to api-rs, which validates the payload
against the tool's manifest, renders the description server side, and
returns:

```json
{"status": "queued", "action_id": "act_01J...", "provisional_id": "pending:act_01J..."}
```

The method's docstring tells the agent what a queued result means. The agent
cannot treat it as success-with-data, and the tool should say so.

Rendering the description in api-rs rather than the sandbox matters. The
sandbox is untrusted, so a description it computed could lie about its own
payload. api-rs holds the tool's manifest and renders from the stored
payload, which is also what gets applied.

The renderer enforces the same rules as the overlay prototype:

- Titles come from the kind.
- Labels are plain text.
- Values that can't be shown as themselves (invisible or bidi characters,
  Slack control tokens such as `<!channel>` or `<url|label>`, fence
  breakouts) are shown escaped.
- A cut over the byte budget is always announced, and the card drops its
  "complete" marker.

### 2. Enforcement (iron-proxy secret rules)

For a gated integration, the console renders two rule sets from one secret:

| Context | Rules on the secret |
|---|---|
| Turn (sandbox proxy) | Read endpoints only, e.g. `methods: [GET]` for Gmail, or `paths: [/api/conversations.*, /api/users.*]` for Slack |
| Apply (control-plane executor) | The single write endpoint the approved action targets |

A sandbox that skips the SDK and sends the placeholder straight to the write
endpoint gets the placeholder forwarded unswapped, and the provider returns
401. No new iron-proxy transform is needed, and the default stays fail
closed.

The integration's manifest declares the read/write split. The console
refuses to mark an integration gated unless every declared action's endpoint
falls outside the turn rule set.

### 3. Storage

A `session_actions` table in api-rs:

| Column | Notes |
|---|---|
| `id` | `act_` ULID; doubles as the provider idempotency key where supported |
| `session_id`, `execution_id`, `thread_key` | where it was queued |
| `principal_id`, `requester_principal_id` | whose credential applies it; who may approve (RFC 0005) |
| `tool`, `method`, `kind`, `delivery` | from the manifest |
| `payload` | `jsonb`, capped (64 KiB); file bytes go to object storage by digest |
| `description` | rendered fields plus `complete: bool` |
| `payload_sha256` | shown on the card and re-checked at apply |
| `status` | `pending`, `approved`, `rejected`, `expired`, `applying`, `applied`, `failed`, `unknown` |
| `decided_by`, `decided_at`, `result`, `expires_at` | |

Pending actions expire after 24 hours by default. Expiry updates the card.

### 4. The approval card (slackbotv2)

When `session.execution_completed` fires for an execution with pending
actions, api-rs starts one `action-approval` workflow for that execution. The
workflow posts a single card in the thread through `ctx.slack_buttons()`:

- One section per action with its rendered fields (up to 5 inline; beyond
  that, a summary plus a "Review" button that opens a modal listing all of
  them).
- Buttons: **Approve all**, **Reject all**, **Review**.
- A footer naming who can approve.

The workflow then waits on `ctx.wait_for_event()` for the decision. On a click, it checks the
clicker's Slack user ID against the allowed approvers:

- the turn's requester, by default;
- or an integration-level allowlist set in the console, for cases like a
  shared lunch order where only named people may approve a charge.

Anyone else's click gets an ephemeral "you can't approve this" and changes
nothing.

`await-decision` actions put the same card up immediately, mid-turn, and the
SDK call blocks until the decision or a timeout (default 10 minutes, then
`rejected`). This is the escape hatch for actions whose result the agent
genuinely needs. It keeps a sandbox warm while it waits, so it should be rare.

### 5. Applying (control plane)

Each approved action becomes a workflow step keyed by the action ID, so
redelivery is idempotent. The step:

1. Re-hashes the stored payload and refuses on mismatch.
2. Runs the tool method's `apply` in a short-lived executor whose proxy
   assignment carries the apply rule for exactly that endpoint.
3. Classifies the outcome, borrowing Cloudflare's three cases:
   - **Retryable:** a plain error where a second attempt is safe; goes back
     to `approved`.
   - **Failed:** the effect is known not to have happened; goes to `failed`.
   - **Unknown:** a timeout or anything after the provider was reached. It
     goes to `unknown`, is never replayed automatically, and shows a "check
     the provider" warning on the card.
4. Updates the card in place with each action's outcome and posts a
   follow-up into the thread so the next turn sees what happened.

Apply runs as the session's principal. The approval adds no authority; it
releases authority the principal already had but was withheld during the
turn.

### 6. What the agent sees next turn

The next execution in the thread gets the decided actions in its session
context (`centaur_sdk.session_actions()`): kind, status, result, and the
provider IDs for applied ones. This matters most for a rejected action,
because the agent shouldn't build on work that never happened.

## Phasing

**Phase 0 (shipped in one deployment's overlay):** prompt-level approval
cards with literal fields and a payload hash. No enforcement.

**Phase 1:** queue, card, apply, and enforcement for write-only actions with
no dependencies. Start with 2 integrations whose reads and writes split
cleanly by path: Slack `chat.postMessage` to channels other than the invoking
thread, and Gmail send.

**Phase 2:** dependencies and auto-approval.

- *Dependencies:* a queued action may reference another's `provisional_id`
  (create a Drive folder, then write a Doc into it). Apply binds provisional
  IDs to provider IDs in order. Rejecting a parent strands its dependents,
  which are marked rejected with the reason.
- *Auto-approval:* two keys. The tool author marks a kind auto-approvable,
  and a console admin enables it per principal. An auto-applied action is
  attributed to the admin who enabled the rule.

**Phase 3:** read projection for providers where the agent routinely reads
back what it wrote. Each tool owns its own projection; there's no generic
version.

## Alternatives Considered

**Status quo, prompt-level confirmation.** No infrastructure. It also costs a
turn per confirmation and provides no enforcement (see Motivation).

**Hold requests inside iron-proxy.** This would gate every write with no SDK
changes. It needs a hold transform upstream in `ironsh/iron-proxy`, plus a
per-provider stand-in response, plus rendering raw HTTP bodies into something
an approver can read. A gzip-encoded multipart MIME email isn't something a
person can approve. Declaring actions at the tool layer gives every action a
human-readable description for free, and the secret rules close the bypass.

**iron-proxy `judge`.** An LLM allow/deny decision is a second opinion, not
an approval. It can only reject, and the human never sees the action.

**Synchronous approval inside the turn for everything.** This is
`await-decision` everywhere. It's simple, but it holds a sandbox for as long
as the human takes to answer. It also brings back the problem the queue
exists to fix: you walk away and come back to an agent stuck on step one.

## Open Questions

1. **Sandbox identity on the queue endpoint.** Sandboxes reach api-rs at
   `CENTAUR_API_URL`. The queue endpoint must bind an action to the calling
   session's execution, and it must refuse a sandbox claiming another
   session. Is there a per-execution credential to reuse, or does this need
   one?
2. **Providers that don't split by path.** GraphQL providers (Linear, GitHub
   GraphQL) and RPC-style APIs where reads are POSTs to the same path. They
   could stay ungated, or gate on a body-matching rule (iron-proxy matches
   host, method, and path today, not body), or always run in
   `await-decision`.
3. **Apply executor.** Should it be a fresh minimal sandbox with only the
   tool and the apply rule, a workflow-python step that calls the tool
   directly, or a warm sandbox with its proxy config swapped? The first is
   cleanest. The last is cheapest.
4. **Requester absent.** Workflow-started executions have no requester, so
   who approves them? Probably a per-workflow approver list declared with the
   workflow, falling back to a console-configured default.
5. **Card size.** Slack caps a message at 50 blocks and a section at 3,000
   characters. A turn that queues 20 actions needs the modal path from day
   one, or a cap on actions per execution.

## Rollout

Behind a per-integration `gated` flag in the console, off by default. Turning
it on for an integration with an existing undeclared write path (an ungated
tool method that POSTs to a write endpoint) breaks that method with a 401.
That breakage is intended, but it needs a console warning listing the
affected methods before the flag flips.
