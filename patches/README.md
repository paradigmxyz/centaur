# Chat SDK Patches

This change upgrades only `services/slackbotv2` to Chat SDK 4.40.0. Other bot
services retain their existing 4.31.0 dependencies and patches. Register patches
by exact version in `pnpm-workspace.yaml`; regenerate with `pnpm patch` and
`pnpm patch-commit`.

## Slack 4.40.0

The Slack patch targets the published ESM entrypoint. It retains three groups
of behavior:

- Trigger-bot mention routing and bot classification when Slack omits `bot_id`.
- Markdown and aggregate task/message budgets, task updates on their owning
  messages, and finalization of outstanding progress on rotation or completion.
- Delivery-error annotations consumed by the live renderer and durable recovery.

Upstream owns mention resolution, native fallback, cancellation, and age
rotation. The old paragraph-preservation and `bot_profile.user_id` fallback
hunks are provided upstream. `SLACK_STREAM_SEGMENT_MAX_AGE_MS` maps to the
upstream adapter option. `SLACK_STREAM_SEGMENT_TASK_CHAR_BUDGET` and
`SLACK_STREAM_SEGMENT_PAYLOAD_CHAR_BUDGET` remain local safeguards; their exact
values are conservative policy, not protocol guarantees.

## Simplified Task Cards

The service caps details at 256 characters, including its truncation notice,
and omits task output before passing chunks to the adapter. The patch bounds
IDs and titles, but no longer splits a task into numbered parts. Multipart
IDs, part-count tracking, and output splitting have been removed. This gives
shorter progress details while preserving final-answer text.

Aggregate budgets remain necessary when a run produces many distinct tasks.
Task ownership also remains: updates must reach the message containing that
task, and open cards must finish when the stream ends. Upstream age rotation
can replay outstanding tasks in a later message.

## Final-Answer Recovery

Delivery errors expose `slackAnswerLost` and `slackStreamMessageId` to
`services/slackbotv2/src/index.ts`. These determine whether a durable final
answer needs replacing or reposting. Cleanup must not flush a rejected append
from the writer's local buffer: it could publish the answer just before fallback
posts it again. After an append failure, the patch uses the server-side stop
endpoint to finalize confirmed content.

## Postgres Compatibility

Slack uses unpatched state-pg 4.40.0, which includes the conditional upsert for
expired cache keys. The state-pg 4.31.0 patch stays registered for the unchanged
services. No state schema migration is introduced.

The matching upsert was verified against real Postgres during the upgrade.
The staging checklist covers application-level recovery; the repository does
not retain a separate test of upstream's SQL implementation.

## Regression Coverage

`test/chat-sdk-emulate.test.ts` covers signed ingress, task-card updates, size
and age rotation, stop/steering, and durable final-answer recovery.
`test/adapter-stream.test.ts` exercises the real SDK and Web API writer against
controlled HTTP failures, including rejected buffers and interrupted sources.
`test/attachment-bytes.test.ts` covers the new ArrayBuffer download result.
Paths in this paragraph are relative to `services/slackbotv2`.

See [Staging Validation](chat-sdk-staging.md) for the remaining real-platform
checks. Automated endpoints do not prove that Slack accepts every local budget.
