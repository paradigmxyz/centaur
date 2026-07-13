# Linearbot Guide

## Role

Linearbot maps Linear comment threads and owned issue assignments to durable
Centaur sessions. It intentionally uses ordinary Comment and Issue webhooks;
Linear native agent sessions are not the primary interaction model.

Key modules are `src/comment-bot.ts`, `src/issue-comments.ts`,
`src/linear-context.ts`, `src/linear-status.ts`, and `src/session-api.ts`. Read
`README.md` and the registered Linear adapter patch before changing event or
thread behavior.

## Invariants

- Verify the Linear webhook HMAC before dispatch. Await the durable
  create/append handoff for user input so transient failures receive a retryable
  status; execute and render after acknowledgement.
- Comment threads and issue-assignment sessions have different keys and
  ownership. Keep the issue-level assignment session as the sole automated
  status owner; comment turns must not race it or apply status markers.
- Plain comments in active threads append context without starting a turn.
  Preserve mention detection, active-thread checks, and self-message loops.
- Issue webhooks should run only when the relevant assignee/delegate field
  changed, not for unrelated issue updates or the bot's own status write.
- Preserve the deliberate adapter patch and workspace patch registration. If
  upstream behavior changes, add a regression test before modifying or removing
  the patch.
- Keep comment edits, reactions, issue fetches, and status updates best-effort
  and bounded without hiding a failed durable execution.

## Validation

From the repository root:

```bash
pnpm --filter linearbot run check:types
pnpm --filter linearbot test
```

The test suite includes a signed-webhook/fake-GraphQL path; extend it for
changes to comment threading, assignment gating, status ownership, or session
API retry behavior.
