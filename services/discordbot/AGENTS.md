# Discordbot Guide

## Role

Discordbot is an outbound Discord Gateway client with a small health server. It
turns allowed mentions and subscribed thread messages into durable Centaur
sessions and renders append-only progress and answers in Discord threads.

Key modules are `src/gateway.ts`, `src/discord-allowlist.ts`,
`src/discord-threading.ts`, `src/discord-narrator.ts`, and
`src/session-api.ts`. See `README.md` for the platform setup and behavior.

## Invariants

- Run one Gateway consumer per bot token. Multiple replicas duplicate message
  handling; preserve the single-replica/Recreate deployment contract.
- Discord Gateway traffic is outbound. Do not add a public webhook route as a
  substitute, and keep health tied to the Gateway controller's actual state.
- Guild access is fail-closed. Empty guild policy means no work; DMs and bot or
  webhook authors remain denied unless explicitly allowed by the existing
  policy.
- A channel mention creates or selects the correct thread; a mention or
  follow-up inside a thread must retain one stable session key. Preserve loop
  guards and per-thread concurrency control.
- Respect Discord rate and content limits. Keep narration append-only and
  throttle answer edits; a rendering refactor must not reorder the final answer
  above later reasoning or user messages.
- Gateway reconnect/resume and redelivered messages must not create duplicate
  executions. Test deduplication separately from thread creation.

## Validation

From the repository root:

```bash
pnpm --filter discordbot run check:types
pnpm --filter discordbot test
```

The unit suite uses fakes and needs no Discord credential. Add focused coverage
for allowlist, threading, Gateway lifecycle, narration, and session API changes.
When external validation is in scope and a controlled test application is
explicitly authorized and configured, verify one input produces one thread,
one execution, and one terminal answer.
