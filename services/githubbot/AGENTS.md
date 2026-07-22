# Githubbot Guide

## Role

Githubbot handles GitHub issue/PR conversations, review requests, issue work,
and explicitly owned PR lifecycle events. Comment events use the Chat SDK
adapter; lifecycle events are verified and handled directly.

Key modules are `src/authorization.ts`, `src/turn.ts`, `src/review.ts`,
`src/issue-manager.ts`, `src/pr-manager.ts`, and `src/session-api.ts`. The
behavioral contract and supported webhook events are documented in `README.md`.

## Invariants

- Verify `X-Hub-Signature-256` before parsing or dispatching every directly
  handled lifecycle event. Preserve delivery-id deduplication.
- Comment-driven work is authorization-sensitive because the sandbox can
  write. Keep the author-association gate fail-closed and retain self-message
  and bot-loop guards.
- Conversation, review, issue-work, and PR-management session keys are
  intentionally isolated. Do not collapse them or let concurrent turns share
  mutable git state accidentally.
- Serialize turns for one management session and drain accepted work on
  shutdown. A webhook acknowledgement is not evidence that the claimed turn
  finished.
- Automated merge behavior applies only to explicitly owned PRs and must honor
  draft, hold, mergeability, settled-check, attempt-limit, and human-handoff
  gates. Keep deterministic merge decisions outside the agent prompt.
- Bundled prompts remain generic and fully overrideable. Do not embed private
  review rules, repositories, user handles, or deployment behavior in defaults.
- Unit tests must not write to real repositories, comments, branches, or PRs.

## Validation

From the repository root:

```bash
pnpm --filter githubbot run check:types
pnpm --filter githubbot test
```

Add focused tests for authorization, signature rejection, deduplication,
session serialization, review/issue triggers, and every PR-manager decision
branch changed.
