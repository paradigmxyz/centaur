---
name: e2e-testing
description: "Write, run, and debug Centaur's end-to-end tests under e2e/. Use when adding or changing an e2e scenario, running the e2e stack locally, or investigating a failing e2e CI job."
---

# Centaur E2E Testing

## How the system works

`e2e/stack.sh` deploys the real chart's Slack path (Postgres, console, api-rs, slackbotv2, sandboxes, per-sandbox iron-proxies) to a dedicated kind cluster, with two fakes standing in for the outside world:

- `e2e/fakes/fake-slack.ts`: a stateful Slack Web/Events/Interactivity API. slackbotv2 runs unmodified against it.
- `e2e/fakes/model-server.ts`: a scripted model provider. CoreDNS sends provider hostnames to it, so real harnesses reach it through iron-proxy. It answers by a token in the user's message and records every request.

Tests in `e2e/tests/*.test.ts` run under `bun test` on the host and reach the fakes, Postgres, and api-rs through NodePorts. `e2e/lib/index.ts` is the test vocabulary: `slack`, `model`, `api`, `cluster`, `workflows`, `ironControl`, and `Thread`. Shared Slack identities live in `e2e/fakes/slack-fixture.ts`. Stack configuration lives in `e2e/infra/`.

CI (`.github/workflows/publish-images.yml`) runs every scenario against the published images with API keys, and runs `harnesses.test.ts` alone in subscription (`access_token`) mode.

## Running

```bash
e2e/stack.sh up                                   # build images from the tree, deploy
e2e/stack.sh test                                 # all scenarios
e2e/stack.sh test e2e/tests/turns.test.ts         # one file
e2e/stack.sh logs                                 # pods, events, component logs
e2e/stack.sh down
```

The script uses a private kubeconfig, never the ambient context. Rerun `up` after changing service code. `CENTAUR_E2E_IMAGE_TAG` pulls published images, and `CENTAUR_E2E_AUTH_MODE=access_token` selects subscription auth. Run `down` before switching from `access_token` back to `api_key`.

## Writing a scenario

- Act only through the doors a user or operator has: Slack for input and visible output, durable session tables, recorded model requests, api-rs operator routes, and Kubernetes faults. Do not reach into service internals.
- Script each turn with `model.says(...)` or `model.fails(...)`, then drive it with `slack.mention(...)`, `thread.mention(...)`, and `thread.nextTurn()`. `nextTurn` waits for a terminal execution and exactly one finished Slack reply.
- Assert observable outcomes: `turn.reply`, `turn.execution.status`, `turn.sandbox`, and `turn.request` (what the harness actually sent the provider).
- Use `test.concurrent` with a `turnTimeoutMs`-based timeout. Each test makes its own thread and script tokens, so tests must not share state.
- Wait with `eventually(...)`, not fixed sleeps. Use `delayMs` plus `thread.inFlight()` to keep a turn running.
- Put a short comment at the top of each file stating the behavior it guarantees. Add a scenario to the existing file that covers that area before creating a new file.
- Extend `e2e/lib` or a fake only when a scenario needs a new capability, and name helpers in user terms. When a test needs new stack configuration, add it to `e2e/infra/values.yaml`, and keep fixture values aligned with `stack.sh`.
- E2E scenarios are slow. Cover cross-service behavior here and leave logic that a unit test can reach to unit tests.

## Debugging failures

Failures name the execution and the replies seen. Next, check `e2e/stack.sh logs` (CI dumps the same output), then the `session_executions` and `session_events` rows for the thread key, then the model server's recorded requests.
