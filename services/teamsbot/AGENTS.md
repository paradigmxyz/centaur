# Teamsbot Guide

## Role

Teamsbot receives Bot Framework activities, applies tenant/team/channel policy,
forwards normalized messages and attachments to durable Centaur sessions, and
renders events back through stored conversation references.

Key modules are `src/teamsbot.ts`, `src/session-api.ts`,
`src/session-transport.ts`, `src/state.ts`, `src/reply-sink.ts`, and
`src/teams-attachments.ts`. See `README.md` for runtime settings.

## Invariants

- Tenant, team, and channel access is fail-closed. Empty allowlists do not mean
  public access, and mention requirements must be enforced before session work.
- Preserve Bot Framework authentication and redact service URLs, access tokens,
  credentials, and attachment authorization from logs and error messages.
- Conversation references, active execution state, and render obligations are
  durable recovery data. Leases and retries must prevent two replicas from
  delivering the same terminal answer.
- Persist the message before execute and keep retryable API failures distinct
  from invalid activities. Do not acknowledge accepted work as complete.
- Attachment download is opt-in, HTTPS-only, size-bounded, and host-allowlisted.
  Validate redirects and authenticated Graph-backed downloads against the same
  restrictions.
- This package uses NodeNext module resolution; keep explicit `.js` suffixes in
  TypeScript imports where the existing code requires them.

## Validation

From the repository root:

```bash
pnpm --filter teamsbot run check:types
pnpm --filter teamsbot test
pnpm --filter teamsbot simulate -- "Reply exactly PONG."
```

The simulator uses a mock Centaur API and needs no Teams credential. Add tests
for allowlists, activity normalization, state recovery, render leases, retry
classification, and attachment limits when those paths change.
