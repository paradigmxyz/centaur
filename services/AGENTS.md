# Service Development Guide

These instructions apply to all tracked services. Also follow the repository
root guide and the nearest service-local `AGENTS.md`.

## Shared rules

- Keep service ownership explicit. Chat services own transport and rendering;
  `api-rs` owns durable orchestration; the sandbox owns harness adaptation; the
  workflow host executes Python handlers but does not own durable state.
- Treat HTTP routes, NDJSON/SSE shapes, thread keys, database rows, environment
  variables, health checks, metrics, and shutdown behavior as contracts.
- When a runtime setting, port, route, probe, secret reference, or network path
  changes, inspect the matching files under `contrib/chart/`.
- Use structured logs with stable event names and correlation fields. Never log
  authorization headers, cookies, tokens, secret values, raw credential
  payloads, or unnecessarily large webhook bodies.
- Preserve fail-closed policy gates. Authenticate or verify signatures before
  accepting untrusted input, and keep allowlists and attachment restrictions
  conservative.
- Add focused regression coverage next to the behavior changed. For a
  cross-service contract, test both sides or provide an integration test that
  crosses the boundary.
- For credentialed tools, prove the full path: declared tool metadata ->
  principal/role authorization -> control-plane and proxy sync -> injected
  outbound request. Confirm the sandbox contains placeholders, not real secret
  values.

## Chat ingress services

Chat integrations should verify the platform event, derive a stable thread
identity, persist the message, start or append to the durable session, and
render replayable events. Do not move sandbox lifecycle, harness translation,
workflow durability, or credential resolution into an ingress service.

Keep these failure boundaries distinct:

- webhook acknowledgement versus background execution completion;
- message persistence versus execution start;
- execution completion versus final platform delivery;
- retryable transport failures versus permanent validation failures;
- deduplication versus per-session serialization.

Tests should cover signature/auth rejection, self-message loops, duplicate
deliveries, session API failures, replay/reconnect behavior, and terminal render
outcomes where applicable.

All TypeScript services belong to the root pnpm workspace. Install dependencies
once from the repository root with `pnpm install --frozen-lockfile`; their
scripts invoke Bun for runtime, tests, and type checking. Do not create nested
lockfiles. If Chat SDK behavior is unclear, inspect `~/github/vercel/chat` and
the repository's registered patches, not `node_modules`.

## Validation

After unit checks, use the root local-stack flow when behavior crosses process,
database, proxy, sandbox, or platform-emulation boundaries. Do not use a remote
deployment as a development test environment.
