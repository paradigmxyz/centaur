---
title: How is Centaur securing my secrets?
description: "Centaur's threat model and the mechanisms that defend against it: sandbox isolation, NetworkPolicy egress restriction, and iron-proxy secret binding."
---

# How is Centaur securing my secrets?

Centaur runs untrusted code on behalf of users: agent harnesses execute
model-generated commands, tools fetch and act on external data, and
prompts can be influenced by anyone whose content reaches the thread.
This page describes what Centaur defends against and the mechanisms
that do the defending.

## Threat model

The realistic threats are:

- **Prompt injection and adversarial inputs.** A malicious instruction
  in a Slack message, a tool response, a webpage the agent fetched, or
  a file in the sandbox can cause the agent to take unintended actions:
  exfiltrate data, call a sensitive tool, or attempt to reach an
  attacker-controlled host.
- **Compromised or malicious dependencies.** Tool code, agent harness
  binaries, or libraries pulled into the sandbox could try to phone
  home, mine credentials, or open a reverse shell.
- **Credential abuse.** Anything that does run in the sandbox has the
  potential to try to extract, log, or misuse the credentials a tool
  needs to do its job.

Centaur is not trying to defend against a fully privileged attacker
already on the host or in the cluster control plane. The model is
defense in depth for what runs inside the sandbox.

## Mitigations

### Sandbox isolation

Each thread runs in its own Kubernetes pod, created and torn down by
the API. Pods are short-lived and run a restricted container security
context (no privilege escalation, all capabilities dropped). Code
that runs in the sandbox cannot reach other sandboxes' filesystems,
processes, or networking.

### Network policy

The Helm chart applies a default-deny NetworkPolicy to every pod in
the namespace. Sandbox pods can only communicate with the Centaur
API and their own dedicated per-sandbox iron-proxy pod. Nothing else
in the cluster or on the internet is directly reachable. Because
iron-proxy is per-sandbox rather than shared, a compromise of one
sandbox's proxy cannot leak into another sandbox.

### Egress policy

All outbound traffic from the sandbox routes through iron-proxy, so
egress policy is enforced in one place. By default the policy is
open.

To lock egress down, edit
[`iron-proxy.yaml`](https://github.com/paradigmxyz/centaur/blob/main/services/iron-proxy/iron-proxy.yaml)
and replace:

```yaml
transforms:
  - name: allowlist
    config:
      domains:
        - "*"
```

with the explicit list of hostnames (or globs like `*.anthropic.com`)
your tools actually need. iron-proxy will reject everything else with
a 403. See the [iron-proxy configuration reference](https://docs.iron.sh/reference/configuration/)
for the full set of allowlist options.

### Credentials

Tool and harness credentials never reach the sandbox. Tools declare
their secrets in `pyproject.toml`:

```toml
[tool.centaur]
secrets = [
    {type = "http", name = "WAREHOUSE_API_KEY", match_headers = ["Authorization"], hosts = ["warehouse.internal.example.com"]},
]
```

The Centaur Console assigns each sandbox proxy to a principal derived from its
user, channel, issue, conversation, or workflow. The proxy receives only the
secrets granted directly to that principal or inherited from its roles. See
[Advanced Permissioning](/secrets/advanced-permissioning) for the setup and
verification workflow.

Three properties of this declaration matter:

- **Placeholders, not values.** The sandbox sees the literal string
  `WAREHOUSE_API_KEY`. iron-proxy substitutes the real credential on
  outbound requests; the value is never present in the sandbox's
  environment, files, prompts, or logs.
- **Bound to specific hosts.** The substitution only happens for the
  hosts listed in `hosts`. A leaked placeholder cannot be redirected
  to an attacker-controlled host.
- **Bound to specific locations.** `match_headers`, `match_query`, or
  `match_path` constrain where the placeholder is allowed to appear.
  The placeholder cannot be smuggled out in a different field or
  header.

Other typed variants extend the same boundary in different ways:

- **`oauth_token`** resolves the declared OAuth credential fields from the
  secret source, exchanges them for an access token, caches and refreshes that
  token, then injects it as `Authorization: Bearer ...` for matching hosts. The
  sandbox never sees the client secret, refresh token, or minted access token.
- **`gcp_auth`** resolves a Google service-account keyfile, mints Google OAuth
  tokens for the configured scopes, and injects those bearer tokens for the
  configured Google API hosts.
- **`pg_dsn`** resolves the real upstream Postgres DSN inside iron-proxy. The
  sandbox receives a local DSN that points at its per-sandbox proxy listener,
  so tool code can connect normally without receiving the real database URL.

See [Creating Tools](/extend/tools) for the full schema.

### Audit trail

Every agent turn (user input, sandbox assignment, execution,
streamed events, tool calls, final delivery) is persisted in
Postgres. iron-proxy emits structured logs for every outbound
request, including which secret was substituted and which transforms
ran. Together they make it possible to reconstruct what an agent did
and what credentials it reached for.

## What this does not protect against

A few honest caveats:

- **A shared backing store still requires careful grants.** Credentials may
  live together in one Kubernetes Secret or 1Password vault, but the Centaur
  Console scopes their use by principal, role, and request rules. The `infra`
  role is assigned to new principals by default so they can run a harness.
  Operators must configure tool roles, direct grants, default roles, and
  sandbox capabilities for their environment. See
  [Advanced Permissioning](/secrets/advanced-permissioning).
- **The default egress allowlist is permissive.** Leaving it open is
  a deliberate UX choice. An open configuration lets users start
  using agents immediately and develop an allowlist over time. If
  you want maximal security, lock down the allowlist up front.
- **Agents have broad permissions inside the sandbox.** They can
  read and write the sandbox filesystem, run shell commands, and
  call any Centaur tool their API token allows. The containment is
  at the sandbox boundary, not inside it.
- **Undesirable agent behavior in general.** Network and credential
  controls limit the blast radius (real keys cannot leak, credentialed
  calls cannot be redirected) but they do not prevent the agent from
  doing something unwanted with the capabilities it legitimately has,
  whether the cause is prompt injection, a confused model, an
  over-eager harness, or buggy tool code. Tool design, especially for
  destructive operations, should assume the agent will occasionally do
  the wrong thing.
