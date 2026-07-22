# Python Workflow Host Guide

## Role

This service is the compatibility runtime for Python workflow handlers. It
discovers workflow modules, invokes `handler(input, ctx)`, and speaks newline-
delimited JSON with `api-rs`. `api-rs` and Postgres own scheduling, leases,
checkpoints, waits, retries, child runs, and terminal state.

## Invariants

- Stdout is protocol-only: exactly one JSON object per line. Send diagnostics
  to stderr and structured workflow messages through `ctx.log`.
- Route durable actions through context RPC (`ctx.step`, sleeps, events, child
  workflows, agent turns, and tool calls). Do not add local checkpoint files,
  background schedulers, or a second workflow state machine.
- A checkpointed step must be replay-safe. Put external side effects inside a
  uniquely named step and keep names stable after deployment.
- Discovery is deterministic. Ignore non-workflow modules, reject duplicate
  workflow names, preserve allowlist behavior, and report load failures without
  corrupting the NDJSON stream.
- Keep the host usable both inside the sandbox image and in local tests. Avoid
  dependencies on a contributor's shell state or remote services.
- Python targets 3.11+. Keep imports at module scope, use absolute imports from
  `api`, and keep serialized results JSON-compatible.

## Validation

From the repository root:

```bash
uv run --project services/workflow-python python -m unittest discover \
  -s services/workflow-python/tests -p 'test_*.py'
```

For protocol changes, run the host as a subprocess and assert clean stdout,
request/response correlation, stderr diagnostics, and shutdown behavior. For a
context primitive change, add matching Rust-side coverage in `api-rs` and run a
small workflow through the local stack. The host ships in both the `api-rs` and
sandbox images rather than as an independent image, so validate both local and
sandboxed host modes when its shared runtime behavior changes.
