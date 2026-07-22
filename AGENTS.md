# Centaur Agent Guide

## Scope and instruction hierarchy

This file applies to the whole repository. Read the nearest `AGENTS.md` before
changing files below it; service-local instructions extend this file and take
precedence for that service.

Keep new and rewritten service guidance deployment-neutral. Do not add new
company names, private domains, cluster names, chat workspace identifiers,
private repository names, absolute user paths, or private overlay procedures.
Use neutral placeholders for new examples. Do not remove or rewrite existing
product-specific defaults solely to make them neutral unless the user asks.

## How to work here

- Inspect `git status` before editing. Preserve unrelated work and never format,
  stage, or revert files outside the task.
- For a focused PR when the current checkout has unrelated changes, use an
  isolated worktree based on the intended branch. Check the base branch and any
  dependent PRs before implementing instead of rebuilding work that already
  exists elsewhere.
- Establish the requested boundary before acting: explanation, review,
  diagnosis, implementation, local validation, and remote operation are
  different scopes. Do not turn a read-only request into a change.
- Read the current implementation, manifests, and tests before relying on prose
  documentation. Prefer an existing name, setting, abstraction, or protocol to
  a parallel one.
- Keep clients thin and changes focused. If a contract changes, update every
  producer, consumer, test fixture, and chart value affected by it.
- Never expose credentials in output, logs, fixtures, commits, or command-line
  arguments. Use placeholders and configured secret paths.
- Do not mutate a remote environment unless the user explicitly asks. Local
  testing does not authorize committing, pushing, deploying, or restarting.
- Before any Kubernetes operation, verify the current context and namespace.
  Pass an explicit `--context` for non-local or destructive work; never rely on
  an ambient context when a mistake could affect another environment.
- When the user explicitly requests an artifact such as a PR, CI repair, or
  deployment, carry the authorized workflow through to that artifact and its
  relevant verification instead of stopping after the code edit.
- Concretely, a PR request means validate, commit, push the branch, open or
  update the PR, and return its link. If the user also asks for CI, rollout, or
  dependent-PR follow-through, monitor and repair that requested boundary too.
- Use conventional commit prefixes when a commit is requested: `feat:`, `fix:`,
  `docs:`, `refactor:`, `test:`, or `chore:`.

## Architecture boundaries

The durable request path is:

1. A chat ingress verifies and normalizes a platform event.
2. The ingress creates or reuses a session, appends the durable user message,
   starts an execution, and consumes replayable events.
3. `api-rs` owns session assignment, execution serialization, recovery,
   workflow state, and persistence in Postgres.
4. The sandbox runtime translates neutral content into the selected harness and
   exposes tool CLIs. Harness and tool traffic reaches upstreams through
   `iron-proxy` without materializing real credentials in the sandbox.
5. The ingress renders durable output back to the originating platform.

Ownership by tree:

- `services/api-rs/`: Rust control plane, durable sessions, sandbox backends,
  workflows, auth integration, and telemetry.
- `services/slackbotv2/`, `discordbot/`, `githubbot/`, `linearbot/`,
  `teamsbot/`: platform transport, policy gates, session forwarding, and
  platform rendering.
- `services/sandbox/`: agent image, startup composition, tool installation,
  repo-cache helpers, and runtime prompt. Harness protocol normalization lives
  in `crates/harness-server/`, which is built into the image.
- `services/iron-proxy/`: credential-injecting proxy image and startup config.
- `services/workflow-python/`: Python workflow compatibility host; durability
  remains in `api-rs`.
- `services/console/`: operator UI and credential-control API.
- `tools/`: independently packaged agent-facing CLI plugins.
- `workflows/`: discoverable workflow definitions.
- `contrib/chart/`: Helm wiring, policies, probes, and service configuration.
- `packages/`: shared TypeScript event and rendering contracts used by ingress
  services.

Do not reintroduce legacy control paths alongside the durable session API.
Modern investigations should start with `sessions`, `session_messages`,
`session_executions`, `session_events`, and workflow state, then follow the
final platform-delivery boundary.

## Local development and validation

Centaur is validated on the local Kubernetes stack. Start with the narrowest
relevant unit or integration test, then prove cross-service behavior when a
boundary changed. Run `kubectl config current-context` before the local stack
commands below; `just deploy` uses the ambient Helm/Kubernetes context.

```bash
just up                         # build and start the local stack
just deploy                     # update the local Helm release
just status
just logs <component>
```

For a runtime change requested for publication, local proof means:

1. run the service's format, type, lint, and unit checks;
2. build the affected runtime artifact with the repository's build recipes;
3. deploy it to the local stack;
4. make a real request through the changed path and inspect the durable result;
5. only then commit or push, and only if requested.

For a missing, duplicate, or stalled chat response, trace the full chain:
platform receipt -> session creation -> durable message -> execution -> event
stream -> render obligation -> final platform message. A healthy pod or one log
line is not proof of successful delivery. If investigation and remediation are
both requested, preserve a bounded evidence snapshot before destructive action
when it is safe to do so.

Useful repository-wide checks include:

```bash
pnpm install --frozen-lockfile
helm lint contrib/chart
git diff --check
```

Python code targets Python 3.11+ and uses `uv` for environments and commands.
Follow the local package's import style: service modules generally use
top-level absolute imports, while independently packaged tool CLIs and optional
dependencies may deliberately import lazily. Do not mechanically rewrite those
boundaries. Rust, Ruby, TypeScript, shell, and image-only services have their
own commands in local guides; there is no single repository-wide lint command
that accurately validates every service.

## Tools and workflows

Tool plugins under `tools/` are independently packaged CLIs. Keep secret access
in the client through the SDK placeholder mechanism; do not load dotenv files
in reusable clients. A tool visible to agents needs a `[project.scripts]` entry,
and its CLI wrapper should remain thin. Validate catalog discovery, `<tool>
--help`, and one real command from a local sandbox.

Workflow definitions under `workflows/` declare a unique `WORKFLOW_NAME` and an
async handler. Use durable context primitives for side effects, sleeps, events,
child workflows, agents, and tools; do not add process-local durability. Keep
step names stable and test replay behavior after failures or restarts.

For a credentialed tool change, trace the complete path: tool declaration ->
principal/role grant -> proxy configuration -> controlled request from a real
sandbox. Configuration presence alone does not prove usable or appropriately
scoped access.

## Reviews and incident reports

- For reviews, report concrete findings in severity order with file and line
  references. Passing tests do not prove protocol, authorization, or recovery
  correctness. Do not edit unless asked to resolve findings.
- For incidents, distinguish durable state, observed logs/metrics, live runtime
  state, deployed version/configuration, and user-visible outcome. State what is
  verified versus inferred.
- Check authorization, credential exposure, idempotency, retry behavior,
  cancellation, and crash recovery early when those boundaries are involved.
- For a broad review, split independent protocol, authorization/lifecycle, and
  deployment-wiring passes, then deduplicate and prioritize the findings.

## Canonical references

- `README.md` and `docs/pages/architecture.mdx`: system overview.
- `docs/pages/quickstart.mdx`: local stack and end-to-end smoke path.
- `contrib/chart/values.yaml`: supported deployment configuration.
- Service `README.md` files, where present: behavior and environment variables.
- `services/api-rs/rfcs/`: control-plane and sandbox design contracts.

"Chat SDK" means the Vercel Chat SDK. When adapter behavior matters, inspect
the source checkout at `~/github/vercel/chat` rather than generated files under
`node_modules`.
