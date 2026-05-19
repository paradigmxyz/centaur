---
title: What is Centaur?
description: Centaur is a production control plane for shared AI agents, durable execution, sandbox isolation, approved tools, and credential-safe automation.
---

# What is Centaur?

Centaur is the control plane for teams that want AI agents to do real work inside their own infrastructure. It gives agents durable memory, isolated runtimes, approved tool access, workflow orchestration, and credential-safe outbound calls without turning every Slackbot or integration into a bespoke agent platform.

The pitch is simple: keep the product surfaces thin, and put the hard operational guarantees in one shared system.

## Durable Agent Turns

Centaur records the user turn, runtime assignment, execution request, streamed events, terminal state, and final delivery obligation in Postgres. A client can disconnect, a worker can restart, and the system still has enough state to replay output, recover completion, or retry delivery.

That makes Centaur a better fit for team workflows than an in-memory chat loop. A Slack thread, API client, or workflow run can all use the same durable control-plane protocol: spawn or reuse a runtime, persist a message, enqueue execution, then stream or replay events.

## Isolated Sandboxes

Each conversation is assigned to a Kubernetes sandbox pod that runs the selected harness, such as Amp, Claude Code, or Codex. The API owns runtime assignment, execution serialization, cancellation, recovery, and release.

Sandboxes speak a stable Anthropic-style message format with the API. Harness-specific quirks stay inside the sandbox adapter, so clients do not need to know how each CLI handles text, images, files, or interrupts.

## Approved Tools

Agents call tools through Centaur's API, not through ad hoc local credentials. Tool plugins expose typed REST endpoints, are discovered by the API, and can be extended without changing the core control plane.

This creates a narrow and auditable boundary for agent capabilities. Teams decide which tools exist, how they authenticate, and what methods are available.

## Credential-Safe Automation

Sandboxes only ever see placeholder strings for upstream credentials. Real values live on [iron-proxy](https://docs.iron.sh), bound to specific hosts and headers, and are swapped in on the fly when a request matches. Agents can call GitHub, model providers, data tools, or internal services without raw long-lived secrets sitting in their workspace.

## Durable Workflows

Centaur includes a Python workflow engine for long-running automation. Workflow handlers checkpoint each step, sleep or wait for external events, start child workflows, and run agent turns as part of larger processes.

This lets teams move beyond one-off prompts. A workflow can poll, branch, retry, call tools, wait for a signal, delegate to an agent turn, and resume after process restarts without rebuilding orchestration from scratch.

## Slack And API Surfaces

Centaur keeps clients thin. Slackbot verifies Slack requests, stores or claims events, calls the API, and renders delivery payloads. External integrations use the same API primitives.

That separation matters: Slack formatting, durable execution, sandbox lifecycle, tool access, and final-delivery recovery each live at the layer that can own them cleanly.

## Overlays For Teams

Deployments can layer organization-specific tools, workflows, skills, personas, prompts, and sandbox behavior over the base Centaur repo. The base platform stays generic while each team adds the behavior it needs.

Overlays are ordered, so later entries can override or extend earlier ones without forcing every deployment into a long-lived fork.

<figure className="architecture-figure">
  <img src="/brand/containers.svg" alt="Centaur deployment layout — paradigmxyz/centaur kernel wrapped by an org-level userspace repo and a per-app application repo" />
  <figcaption>Three nested repos: <code>paradigmxyz/centaur</code> is the kernel (control loop, workflow engine, sandboxing), the org's <code>centaur</code> overlay holds shared business logic, and each <code>example-centaur-app</code> sits on top with app-specific workflows wired to its Slackbot.</figcaption>
</figure>

## Production Shape

Centaur is built around a Kubernetes deployment model:

- API control plane for durable agent and workflow state
- Slackbot and other clients as thin adapters
- Sandbox pods for isolated harness execution
- Postgres as the source of truth
- Firewall/proxy credential injection for outbound model and tool calls
- Optional logs, metrics, and dashboards for production observability

Use Centaur when agents need to be shared, recoverable, auditable, and connected to real systems. If a demo script is enough, Centaur is probably too much. If agents are becoming part of production workflows, Centaur gives them a real operating model.
