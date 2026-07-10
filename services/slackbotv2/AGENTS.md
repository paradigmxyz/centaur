# Slackbot v2 Guide

## Role

Slackbot v2 is the Slack transport and renderer for the durable session API.
`src/index.ts` wires Chat SDK callbacks and recovery, `src/session-api.ts` owns
the control-plane client, `src/conflate.ts` and display helpers shape output,
and `src/server.ts` owns runtime configuration and the HTTP server.

Keep this service thin: it may collect Slack context, persist messages, start
or interrupt executions, replay events, and deliver Slack output. Sandbox
lifecycle, harness formatting, and durable execution state belong in `api-rs`.

## Invariants

- Verify Slack signatures and policy gates before processing an event. Ignore
  bot/self events and unsupported event shapes without creating sessions.
- For user-input webhooks, wait only for the create/append handoff needed to
  make Slack retry a transient failure. Do not hold the webhook open for cold
  sandbox execution or rendering.
- Preserve the boundary between create/reuse, durable append, execute, SSE
  replay, and final Slack delivery. Each phase needs its own timeout, retry,
  metrics, and idempotency behavior.
- Use stable client message and execution idempotency keys so Slack redelivery
  cannot produce a second turn. Serialize work that targets the same session.
- Postgres-backed Chat SDK state and render obligations are crash-recovery
  state. A terminal execution without a delivered terminal render still needs
  recovery.
- Keep Slack API side effects bounded by the existing timeout helpers. Respect
  message, block, attachment, and rate limits, including fallback text.
- Avoid serializing raw webhook bodies on the hot path or in normal logs. Never
  log bot tokens, signing secrets, private file URLs, or user file contents.
- Preserve stop commands, harness/model overrides, late-file repair, initial
  thread context, and subscribed-message append semantics when refactoring the
  main callback flow.

## Validation

From the repository root:

```bash
pnpm --filter slackbotv2 run check:types
pnpm --filter slackbotv2 test
```

Use targeted tests while iterating, especially `test/session-api.test.ts`,
`test/chat-sdk-emulate.test.ts`, `test/conflate.test.ts`, and recovery/metrics
tests. For changes to acknowledgement, retry, or rendering, deploy locally and
exercise a signed emulated event through the HTTP route; prove the webhook
status, durable execution count, replayed terminal event, and final rendered
message.
