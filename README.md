<img width="1500" height="500" alt="image" src="https://github.com/user-attachments/assets/cc85cdb1-5a72-4eb2-ba1b-2e0a8fbbf691" />

<h4 align="center">
    Shared AI agents for teams.
</h4>

<p align="center">
  Talk to an agent in Slack, give it your tools, and let it run real work in a sandbox.
</p>

<p align="center">
  <a href="#features">Features</a> •
  <a href="#overview">Overview</a> •
  <a href="#getting-started">Getting Started</a> •
  <a href="#documentation">Documentation</a>
</p>

## Features

- **Slack-native agent conversations**: mention the bot in Slack and get progress plus final answers back in the thread.
- **Real execution environment**: each conversation runs in an isolated Kubernetes sandbox with a shell, workspace, git, Python, Node.js, Bun, and common development tools. For local setup, a lightweight k3s-based cluster is enough; you do not need a full production Kubernetes installation.
- **Bring your own harness**: run CLI-based agents such as Amp, Claude Code, Codex, or deployment-specific harnesses.
- **Shared tools**: add Python tool plugins once and make them available to every agent conversation.
- **Durable workflows**: run jobs that can sleep, resume, wait for events, start child agents, and survive service restarts.
- **Credential boundaries**: agents can use approved services without receiving raw API keys in their environment.
- **Replayable state**: messages, executions, events, and delivery state are stored so clients can reconnect without losing the result.
- **Organization overlays**: layer in your own tools, workflows, personas, skills, and prompts without forking the base platform.

...and a lot more.

## Overview

Centaur is a self-hosted agent platform for teams that want one shared agent instead of many one-off local setups.

```text
# 1. Ask from Slack.
@centaur can you figure out why the billing tests are failing?

# 2. Centaur assigns a sandbox for the thread.
# The agent can inspect code, run commands, and call approved tools.

# 3. Progress and the final answer are delivered back to Slack.
```

You can also drive Centaur directly through the API:

```text
POST /agent/spawn      # assign or reuse a sandbox
POST /agent/message    # store the user turn
POST /agent/execute    # start the agent
GET  /agent/threads/{thread_key}/events
```

The platform handles sandbox lifecycle, durable transcript storage, tool access, credential injection, workflow execution, and final delivery.

## What Centaur Is Good For

- investigating CI failures
- answering questions with internal tools
- summarizing Slack threads or customer context
- running recurring checks and digests
- coordinating multi-step operational workflows
- giving agents access to a real repo and test environment
- standardizing agent behavior across a team

## How It Works

```text
Slack or API
    |
    v
Centaur API
    |
    +-- Postgres durable state
    +-- tool and workflow registry
    +-- sandbox assignment
    |
    v
Kubernetes sandbox
    |
    +-- agent harness
    +-- workspace and shell
    +-- API-mediated tool calls
    |
    v
Controlled outbound access
```

The main directories are:

- [`services/api-rs`](services/api-rs/) — Rust control plane for agents, tools, workflows, auth, and durable state
- [`services/slackbotv2`](services/slackbotv2/) — Slack event handling and Slack delivery
- [`services/sandbox`](services/sandbox/) — agent container image and harness adapter
- [iron-proxy](https://docs.iron.sh) ([service](services/iron-proxy/)) — controlled outbound access and credential injection
- [`tools`](tools/) — tool plugins
- [`workflows`](workflows/) — workflow plugins

## Getting Started

Centaur runs locally on Kubernetes, but local setup does not require a full
production Kubernetes installation. A lightweight k3s cluster on a small
always-on host is enough. The expected local checkout path is:

```bash
/Users/magelinskaas/paradigmxyz/centaur
```

Clone the repo and enter it:

```bash
git clone <repo-url>
cd centaur
```

Install the local command runner:

```bash
brew install just
```

For a first-time local boot, Centaur needs these infrastructure secrets in your shell:

```bash
export OP_SERVICE_ACCOUNT_TOKEN=...
export OP_VAULT=...
export SLACK_BOT_TOKEN=...
export SLACK_SIGNING_SECRET=...
export SLACKBOT_API_KEY=...
```

Create the Slackbot app at [api.slack.com/apps](https://api.slack.com/apps).
Use the app's Bot User OAuth Token for `SLACK_BOT_TOKEN` and its Signing Secret
for `SLACK_SIGNING_SECRET`.

What they are for:

- `OP_SERVICE_ACCOUNT_TOKEN`: lets [iron-proxy](https://docs.iron.sh) resolve 1Password-backed runtime secrets
- `OP_VAULT`: the 1Password vault name or id to read from
- `SLACK_BOT_TOKEN`: Slack bot token for the local Slackbot service
- `SLACK_SIGNING_SECRET`: verifies incoming Slack requests
- `SLACKBOT_API_KEY`: API key the Slackbot uses to call Centaur

Then create local Kubernetes Secrets from those environment variables and boot the stack:

```bash
just bootstrap-secrets
just up
```

After `just up` finishes, use Slack or the API examples in the [Developer Guide](AGENTS.md#e2e-testing-without-slack).

For a more explicit path, see [Running Centaur on a Mac Mini-style setup](docs/pages/mac-mini-setup.mdx).
It covers k3s on a small VPS, DigitalOcean droplet, Linux box, or Mac
Mini-style host.

## Tools

Tools are small Python plugins. A tool can wrap an internal service, public API, database, search endpoint, deployment system, or anything else an agent should be allowed to use.

```text
tools/my_tool/
├── __init__.py
├── client.py
├── cli.py
├── .env.example
└── pyproject.toml
```

Public methods on the client become tool endpoints. For example, `search()` becomes:

```text
POST /tools/my_tool/search
```

## Workflows

Workflows are Python functions with durable steps.

```python
WORKFLOW_NAME = "daily_digest"

async def handler(inp, ctx):
    data = await ctx.step("collect", lambda: collect_digest_data(inp))
    summary = await ctx.run_agent("summarize", text=f"Summarize this: {data}")
    return {"summary": summary}
```

Use workflows when a task should run longer than one request, wait for something external, run on a schedule, or coordinate multiple agent turns.

## Security Model

Centaur is designed around practical isolation and auditability:

- each conversation runs in a Kubernetes sandbox with a default-deny NetworkPolicy
- sandboxes call tools through the Centaur API and reach the outside world only through a per-sandbox [iron-proxy](https://docs.iron.sh)
- sandboxes only ever see placeholder strings for upstream credentials; real values are swapped in by iron-proxy only on outbound requests to the specific hosts and headers a secret is bound to
- messages, executions, events, and delivery state are persisted
- outbound activity can be audited

See [Security](docs/pages/security.mdx) for the full threat model and the mechanisms behind each of these points, including known limitations like the shared credential vault.

## Documentation

- [Docs app](docs/) — Vocs documentation source
- [Quickstart](docs/pages/quickstart.mdx) — boot the local Kubernetes stack and run one agent turn
- [Deploying in Production](docs/pages/deploying-in-production.mdx) — production secrets, Slack, Helm, and verification
- [Architecture](docs/pages/architecture.mdx) — control plane, sandbox runtime, tools, workflows, and credential injection
- [Developer Guide](AGENTS.md) — full local setup, architecture, API contracts, migrations, testing, and conventions
- [Tools](tools/) — built-in tool plugins
- [Workflows](workflows/) — external workflow plugins
- [API service](services/api-rs/) — Rust control plane
- [Slackbot](services/slackbotv2/) — Slack integration
- [Sandbox](services/sandbox/) — agent runtime image

## Contributing

Before pushing changes, build and test the affected service locally:

```bash
just build-one <service>
just deploy
```

For Python checks:

```bash
uv run ruff check .
uv run ruff format .
uv run pytest
```

See the [Developer Guide](AGENTS.md) for code conventions and end-to-end testing instructions.

## Acknowledgements

Centaur builds on excellent open-source infrastructure, including [FastAPI](https://fastapi.tiangolo.com/), [Kubernetes](https://kubernetes.io/), [mitmproxy](https://mitmproxy.org/), and the agent harnesses teams choose to run inside the sandbox.
