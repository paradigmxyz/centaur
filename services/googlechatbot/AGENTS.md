# Googlechatbot Guide

## Role

Googlechatbot receives Google Chat app events (direct webhook) and Workspace
Events notifications (Pub/Sub push), applies space/sender policy, forwards
normalized messages and attachments to durable Centaur sessions, and renders
events back by editing one progress message in the space.

Key modules are `src/googlechatbot.ts`, `src/googlechat-message.ts`,
`src/reply-sink.ts`, `src/session-api.ts`, `src/session-transport.ts`, and
`src/state.ts`. See `README.md` for runtime settings and the Google Cloud setup.

## Invariants

- Inbound verification is fail-closed. The service refuses to boot without a
  webhook audience (`GOOGLE_CHAT_PROJECT_NUMBER` or `GOOGLE_CHAT_ENDPOINT_URL`),
  and Pub/Sub delivery additionally requires both an audience and the pushing
  service account identity.
- Space, sender, and DM access is fail-closed. Empty allowlists do not mean
  public access, and mention requirements must be enforced before session work.
- Attachment `downloadUri` values are authenticated URLs: never persist,
  forward, or log them. Downloads go through the adapter's app-credentialed
  data reference, opt-in and size-capped.
- Rendering edits one message and must stay throttled
  (`GCHAT_RENDER_MIN_EDIT_INTERVAL_MS`): Google Chat rate limits
  `spaces.messages.update`, and an unthrottled token stream exhausts the quota.
  Content over 4096 characters spills into follow-up messages, and overflow is
  only posted at completion because follow-ups cannot be retracted.
- Space references, active execution state, and render obligations are durable
  recovery data. Leases and retries must prevent two replicas from delivering
  the same terminal answer.
- Persist the message before execute and keep retryable API failures distinct
  from invalid events. Do not acknowledge accepted work as complete.
- This package uses NodeNext module resolution; keep explicit `.js` suffixes in
  TypeScript imports where the existing code requires them.

## Validation

From the repository root:

```bash
pnpm --filter googlechatbot run check:types
pnpm --filter googlechatbot test
pnpm --filter googlechatbot simulate -- "Reply exactly PONG."
```

The simulator uses a mock Centaur API and needs no Google credential. Add tests
for allowlists, event normalization (both webhook and Pub/Sub shapes), state
recovery, render leases, edit throttling, and attachment limits when those paths
change.
