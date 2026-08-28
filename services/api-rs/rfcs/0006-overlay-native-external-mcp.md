# RFC 0006: Policy-Enforced Overlay-Native External MCP Tools

Status: Draft
Owner: TBD
Target: `services/api-rs`, `services/sandbox`, `services/console`,
`services/iron-proxy`

## Summary

Let an overlay register a remote Model Context Protocol server by extending the
existing per-tool `pyproject.toml` contract. Centaur should discover one typed
declaration, publish it as versioned control-plane state, render the authorized
server into each supported harness, and compile the same declaration into
principal-scoped iron-proxy route, egress, credential, method, and tool policy.

Version 1 is a remote, tool-only Streamable HTTP profile. It supports the MCP
lifecycle, `tools/list`, and explicitly authorized `tools/call` requests.
Resources, prompts, sampling, roots, elicitation, unrestricted extension
methods, general MCP OAuth, and local stdio servers are deferred until Centaur
can enforce them explicitly.

The proposed unit remains a tool package directory, but its capabilities become
explicit. A directory may provide a CLI, one remote MCP server, or both. An
MCP-only directory does not synthesize a CLI or appear in Centaur's existing
public `/mcp` tool catalog. Matching skills remain instructions only and grant
no CLI, MCP, network, or credential access.

This RFC covers Centaur acting as an MCP client inside agent sandboxes. It does
not replace or modify the existing `/mcp` endpoint, where Centaur acts as an MCP
server for external clients.

## Motivation

The current escape hatch is harness-specific configuration such as
`CODEX_CONFIG_OVERLAY`. It proves a remote MCP server can be reached, but it
does not provide a shared declaration, principal-scoped visibility, managed
proxy policy, route-only credential delivery, or warm-pool lifecycle handling.

Bespoke Python MCP clients solve individual integrations but repeat transport,
session, SSE, error, schema, and protocol handling. A shared declaration should
drive both the native harness configuration and the actual authorization
boundary.

Related work:

- [#1176](https://github.com/paradigmxyz/centaur/issues/1176) describes the
  external-MCP client gap.
- [#1385](https://github.com/paradigmxyz/centaur/issues/1385) explains why
  static `iron-proxy.yaml` changes do not cover Kubernetes managed mode.
- [#205](https://github.com/paradigmxyz/centaur/pull/205) identified protocol
  headers required by stateful Streamable HTTP sessions.
- [#883](https://github.com/paradigmxyz/centaur/pull/883) proposes role-based
  tool visibility in the opposite MCP direction.
- [#841](https://github.com/paradigmxyz/centaur/pull/841) implements Centaur as
  an MCP server.

## Goals

- Declare a remote Streamable HTTP MCP server in an overlay tool directory.
- Make CLI and external MCP capabilities independently discoverable and
  grantable.
- Use one normalized declaration for every supported harness and for managed
  proxy policy.
- Reuse existing secret declarations and source types without granting the MCP
  credential as a generic host-scoped sandbox secret.
- Keep real credentials out of sandbox files, environment variables, process
  arguments, and logs.
- Route authorized MCP traffic through a generated gateway route.
- Apply default-deny MCP method and tool policy at iron-proxy.
- Make visibility follow an explicit session/requester authorization formula.
- Preserve existing CLI, secret, skill, persona, overlay, and warm-pool behavior
  when no external MCP server is assigned.
- Publish source-attributed catalog generations that downstream components can
  reconcile and diagnose.
- Fail a new external-MCP assignment before agent input when proxy and harness
  generations cannot be proven consistent.

## Non-Goals

- Replacing or changing existing CLI, secret, skill, persona, or workflow
  formats.
- Changing existing `tool-<slug>` grants or generic HTTP secret behavior.
- Changing sandbox assignment when no external MCP server is authorized.
- Adding a root overlay manifest.
- Supporting legacy HTTP+SSE transport.
- Supporting arbitrary MCP resources, prompts, sampling, roots, elicitation, or
  extension methods in version 1.
- Running arbitrary install or shell commands from overlay metadata.
- Solving general MCP OAuth discovery, browser consent, or dynamic client
  registration.
- Aggregating external servers into Centaur's public `/mcp` endpoint.
- Treating harness configuration as an authorization boundary.
- Supporting local stdio MCP servers in version 1.
- Retrofitting route-only MCP policy onto existing direct HTTP integrations.

## Proposed Overlay Shape

An anonymous MCP-only package can contain only metadata:

```toml
[project]
name = "ethereum-mcp"
description = "Ethereum developer tools exposed over MCP"
version = "0.1.0"
requires-python = ">=3.11"

[tool.centaur.mcp]
name = "ethereum"
transport = "streamable-http"
url = "https://ethereum-mcp.example.com/mcp"
public = false
allowed_tools = [
  "get_balance",
  "get_block",
  "get_transaction",
  "read_contract",
]
```

No `client.py` or `[project.scripts]` entry is required. Discovery classifies
this as MCP-only and does not add a synthetic executable tool entry.

A package may also expose an ordinary CLI and reuse an existing secret
declaration as the source for a gateway-only credential:

```toml
[project]
name = "chain-tools"
description = "Chain CLI and remote MCP server"
version = "0.1.0"
requires-python = ">=3.11"

[project.scripts]
chain = "chain_tools.cli:app"

[tool.centaur]
module = "client.py"
hosts = ["api.chain-tools.example.com"]

[tool.centaur.mcp]
name = "chain"
transport = "streamable-http"
url = "https://mcp.chain-tools.example.com/mcp"
allowed_tools = ["resolve_name", "get_balance"]
credential = "CHAIN_MCP_TOKEN"

[[tool.centaur.secrets]]
type = "http"
name = "CHAIN_MCP_TOKEN"
secret_ref = "CHAIN_MCP_TOKEN"
mode = "inject"
inject_header = "Authorization"
inject_formatter = "Bearer {{ .Value }}"
hosts = ["mcp.chain-tools.example.com"]
```

`credential` references a same-directory secret declaration. The MCP compiler
reuses its source type, source reference, host scope, and formatter, but the MCP
role compiles it into a gateway-only binding rather than granting the ordinary
generic secret resource. Existing CLI grants and existing secret behavior are
unchanged.

A package that intentionally needs the same value for both direct CLI HTTP and
MCP may use distinct secret names with the same `secret_ref`. Those remain
separate grant and policy identities. A separate existing generic grant is
separate authority and is not constrained by this RFC.

### MCP fields

| Field | Required | Meaning |
| --- | --- | --- |
| `name` | yes | Stable logical, harness, and audit name within the package. |
| `transport` | yes | `streamable-http` in version 1. |
| `url` | yes | HTTPS upstream URL with a DNS hostname and normalized path. |
| `allowed_tools` | yes | Explicit upstream tool names accepted by policy. |
| `credential` | no | Same-directory secret name compiled only into the MCP route. |
| `public` | no | Expose a credentialless server without a role; defaults to `false`. |
| `role` | no | Explicit role foreign ID; defaults to an MCP-specific role. |
| `startup_timeout_sec` | no | Bounded connection/startup timeout. |
| `tool_timeout_sec` | no | Bounded tool-call timeout. |
| `enabled_harnesses` | no | Narrowing-only list of compatible harnesses. |

The URL is declarative so api-rs and Console can derive route identity, egress,
credential scope, and diagnostics without executing overlay code. URL
templates, shell interpolation, fragments, IP-literal hosts, literal
authorization headers, and userinfo are rejected.

The URL supplies the MCP upstream host. Existing `[tool.centaur].hosts` remains
available for ordinary CLI secret behavior but is not required for an MCP-only
package. A referenced credential must be scoped to the normalized upstream host
and, where the existing secret model supports it, the narrowest practical path.

`allowed_tools` is mandatory and non-empty. Version 1 has no allow-all or
argument-condition syntax. A later typed tool-table form may add conditions once
the pinned proxy representation is specified and tested.

`public = true` is accepted only for a credentialless server and remains subject
to an operator-level deployment policy. Credential presence is not used as an
authorization predicate.

## Capability Identity, Authorization, and Discovery

External MCP servers use a separate role by default:

```text
mcp-<package-slug>-<server-slug>
```

This follows the existing role/grant pattern without silently expanding an
existing CLI role. Trusted overlay metadata may explicitly select an existing
role when the operator intends the CLI and MCP surface to be one capability.

For interactive sessions, an MCP server is visible only when:

1. the conversation/session principal holds the MCP role; and
2. when a requester principal is present, the requester also holds the role.

Workflow, tool-host, and other sessions without a requester use their service
principal alone. Requester-hoisted or always-available MCP roles require a
separate explicit design; they are not inferred from credential hoisting.

Discovery produces independent package capabilities:

```text
DiscoveredPackage {
  package
  source
  cli?
  external_mcp?
}

ExternalMcpServer {
  name
  package
  overlay
  transport
  upstream_url
  upstream_host
  allowed_tools
  credential_ref?
  public
  required_role
  startup_timeout?
  tool_timeout?
  enabled_harnesses?
  source_repo
  source_ref
  source_commit
}
```

An MCP-only package contributes `external_mcp` but no `cli`. It is not inserted
into existing executable discovery, CLI shim installation, or Centaur's public
MCP tool catalog.

### MCP-specific overlay shadowing

External MCP declarations use the existing ordered overlay inputs but fail
closed independently from current CLI and persona behavior:

- a later package claims its external MCP identity before full validation;
- a valid later declaration replaces the earlier declaration;
- an invalid later declaration creates a diagnostic tombstone and suppresses
  the earlier external MCP declaration; and
- existing CLI, secret, persona, and skill shadowing remains unchanged.

Logical names, display names, and route IDs are distinct normalized fields.
Collisions after normalization are invalid rather than silently merged.

A declaration is invalid when:

- the URL is not HTTPS, lacks a DNS hostname, contains userinfo or a fragment,
  uses an IP literal, or uses an unsupported transport;
- a referenced credential is absent or is not scoped to the upstream host;
- `public = true` is combined with a credential;
- `allowed_tools` is empty, duplicated, or invalid;
- the generated or explicit role is invalid;
- active entries collide after name or route normalization; or
- a timeout or harness name is unsupported.

Ordinary discovery must not contact the upstream or execute `tools/list`.

## Catalog Generations

api-rs owns parsing overlay TOML. Console and iron-control consume an
authenticated normalized catalog generation and must not independently reparse
the manifest.

A generation records:

- source repository, ref, commit, and overlay order;
- normalized entries and tombstones;
- role identities and credential references, never values;
- validation diagnostics; and
- a canonical catalog hash.

Publication is idempotent and source-attributed. One designated reconciler owns
a configured source so competing api-rs replicas cannot publish different
commits as the same generation. Removal is represented by a newer generation or
tombstone, not by silently treating missing input as a valid empty catalog.

Downstream state carries separate hashes:

1. `catalog_hash` for normalized declarations;
2. `grant_hash` for the effective principal authorization set;
3. `proxy_config_hash` for managed proxy policy; and
4. `harness_config_hash` for the final parsed harness configuration.

The hashes may be combined into an assignment generation, but diagnostics retain
the individual values.

## Managed Proxy and Protocol Enforcement

Version 1 exposes a fixed, default-deny tool-only protocol profile. The route
permits initialization and lifecycle messages, ping, cancellation/progress
needed by an authorized call, `tools/list`, and `tools/call` for listed tool
names. Other client-to-server and server-to-client methods are denied.

Filtering `tools/list` is a discoverability feature, not the authorization
boundary. Requests and server-originated JSON-RPC messages are checked before
they reach the other side.

External MCP requires three route-scoped guarantees:

1. **Route-only upstream authority.** The upstream host/path is authorized only
   for traffic entering through the generated MCP route. A direct sandbox
   request to that upstream does not inherit authority from the MCP grant.
2. **Gateway-only credential binding.** A referenced secret is resolved and
   injected by the route after MCP authorization. The MCP grant does not emit it
   into ordinary generic secrets, transforms, or sandbox environment.
3. **Bidirectional method and tool policy.** The fixed method profile and
   explicit tool allowlist apply to both protocol directions.

These are additive external-MCP fields in the managed iron-control and Console
snapshot path. They do not require changing unrelated proxy traffic or the
behavior of existing generic credentials.

For each granted server, managed proxy configuration contains:

- a stable client-facing route and collision-checked route ID;
- an MCP policy matching that route;
- the fixed method profile and explicit tool allowlist;
- a gateway mapping to the HTTPS upstream;
- optional gateway-only credential source metadata;
- the minimum route-specific upstream and token-endpoint egress; and
- catalog, grant, and rendered policy hashes.

The client-facing route must not use the special-use `.local` suffix. It may use
the existing per-sandbox proxy service plus a generated route path or a
deployment-controlled internal DNS suffix.

The route-local HTTP profile preserves the headers and methods required by the
pinned Streamable HTTP protocol, including `Mcp-Session-Id`,
`MCP-Protocol-Version`, `Last-Event-ID`, content negotiation, POST, GET, and
negotiated session termination. Redirects may not escape the declared host and
path policy or carry credentials to a different authority.

The policy order is:

```text
harness
  -> principal's iron-proxy
  -> generated MCP route
  -> route-only upstream authorization
  -> bidirectional method policy
  -> tools/call allowlist
  -> gateway-only credential injection
  -> remote MCP server
```

The sandbox receives only the client-facing URL and proxy CA. Real credential
values never appear in harness configuration or a shared adapter.

## Runtime and Lifecycle

The runtime flow is:

1. api-rs parses CLI, secret, and external MCP metadata into independent
   capability records.
2. MCP-specific shadowing selects valid entries or tombstones.
3. The reconciler publishes the catalog generation.
4. The control plane computes the effective server set from session and
   requester roles.
5. Console/iron-control renders the granted routes and managed proxy policy.
6. The proxy applies and reports the expected `proxy_config_hash`.
7. The sandbox renders, reparses, and applies the harness configuration.
8. Centaur delivers agent input only after the proxy and harness hashes match
   the assignment generation.

The strict barrier is new and applies only to assignments containing external
MCP servers. Existing assignment behavior remains unchanged otherwise.

Harness-specific syntax belongs in `services/sandbox`, not in overlays. The
renderer writes native Codex, Claude Code, Amp, or other explicitly supported
configuration from one normalized JSON representation.

`CODEX_CONFIG_OVERLAY` and `CLAUDE_SETTINGS_OVERLAY` remain operator escape
hatches and apply after generated configuration. They may narrow, rename, or
remove harness entries, but cannot create proxy authority. The final merged
configuration is reparsed; invalid output or an unauthorized route is an
assignment failure.

Each enabled harness adapter must provide one of:

- authenticated hot reload with an applied-generation response;
- controlled restart after assignment and before first input; or
- a stable local broker whose applied catalog generation is queryable.

A warm-pool claim with external MCP is transactional from the session's point
of view:

1. reserve the sandbox;
2. compute the effective assignment generation;
3. apply and verify proxy policy;
4. render and verify harness configuration;
5. expose the harness to input; or
6. mark and remove the sandbox when the barrier fails.

Additions become visible at a successful execution boundary or adapter reload.
Removal and role revocation fail closed: revoke the proxy route first, close
active gateway sessions, deny new calls, and remove harness visibility
afterward. An already-running upstream request may complete, but no new request
may start under the revoked generation.

## Required Invariants

Implementation must preserve these security and compatibility properties:

- Packages without `[tool.centaur.mcp]` retain existing behavior.
- Existing secrets retain existing behavior unless referenced by a new MCP
  route; the MCP grant itself does not grant the generic secret.
- Existing `tool-<slug>` roles do not gain MCP authority by default.
- Existing CLI, persona, skill, and secret shadowing is unchanged.
- MCP-only packages do not synthesize CLI entries or appear in Centaur's public
  `/mcp` catalog.
- Existing warm-pool and assignment behavior is unchanged without external MCP.
- Static-only proxy configuration is not introduced into the Kubernetes path.
- A declaration alone never grants a role or credential to a principal.
- Direct upstream requests receive neither MCP route authority nor the
  gateway-only credential.
- Private, loopback, link-local, and metadata destinations remain denied unless
  a separate trusted internal-host policy explicitly permits them.
- DNS is rechecked at connection time so later resolution changes cannot bypass
  address policy.
- Unknown tools, unknown methods, stale generations, and failed managed-policy
  syncs fail closed.
- Operator harness overlays cannot widen proxy policy.
- External descriptions, schemas, notifications, and results are untrusted
  model input.
- Audit logs may record source generation, principal set, server, method, tool,
  decision, route, and policy version, but not authorization headers, likely
  secret arguments, or response bodies by default.

## Rollout Plan

### Phase 1: Typed discovery

- Parse `[tool.centaur.mcp]` into independent capability records.
- Classify MCP-only packages without changing existing executable catalogs.
- Add validation, fail-closed MCP shadowing, tombstones, and diagnostics.
- Do not publish, render, or execute external MCP.

Exit condition: valid and invalid overlay fixtures produce deterministic,
source-attributed catalogs while existing fixtures remain unchanged.

### Phase 2: Catalogs and grants

- Persist authenticated catalog generations.
- Add MCP-specific roles and the session/requester authorization formula.
- Normalize referenced credentials into gateway-only bindings.
- Expose catalog and grant hashes to diagnostics.

Exit condition: the control plane can deterministically answer which servers
and credential sources a principal set may receive.

### Phase 3: Managed proxy enforcement

- Extend managed snapshots for routes, route-only authority, gateway-only
  credentials, method policy, tool policy, and route-local transport headers.
- Pin an iron-proxy version that enforces the new fields.
- Verify direct-upstream denial and anonymous and credentialed routes.

Exit condition: managed Kubernetes proxies enforce the complete version 1
boundary without harness configuration being trusted.

### Phase 4: Harnesses and lifecycle

- Render Codex first, then Claude Code and Amp.
- Add the strict proxy-and-harness assignment barrier.
- Add warm-pool failure/disposal, refresh, and revocation behavior.
- Enable only harness adapters that can report an applied generation.

Exit condition: end-to-end tests prove cold and warm assignments expose exactly
the authorized server generation and revoke it in the required order.

Local stdio support is a separate follow-up after a shared bridge can enforce
the same bidirectional method and tool policy.

## Validation Plan

Validation is organized by boundary rather than as one exhaustive test list:

- **Discovery:** valid, malformed, shadowed, tombstoned, public, credentialed,
  CLI-plus-MCP, and MCP-only fixtures; name and route collision tests.
- **Compatibility:** unchanged normalized output for packages without new
  metadata; no synthetic CLI or public `/mcp` entries for MCP-only packages.
- **Authorization:** session, requester, and service-principal formulas;
  explicit roles; public policy; ungranted and unresolved credentials.
- **Proxy enforcement:** lifecycle and transport headers; allowed and denied
  methods; filtered and denied tools; gateway-only credential injection;
  direct upstream, redirect, and DNS-rebinding denial.
- **Lifecycle:** cold create, warm claim, proxy lag, harness reload failure,
  overlay refresh, role change, revocation order, and sandbox disposal.
- **End to end:** one controlled anonymous server and one credentialed server
  through the local Kubernetes managed-mode stack.

Implementation PRs should expand these categories into concrete cases for the
component they change.

## Open Questions

- Should catalog publication be owned by a dedicated reconciler or a
  leader-elected api-rs replica?
- Should client-facing routes use the existing per-sandbox proxy service and
  paths or a deployment-controlled internal suffix?
- Which harnesses can report an authenticated applied generation after reload,
  and which require restart?
- Should catalog additions affect existing sessions only at execution
  boundaries, or may an adapter opt into immediate verified reload?
- Which existing secret source kinds are safe for non-interactive gateway-only
  use in version 1?
- Should future MCP OAuth be a Console integration type or an extension of
  brokered credentials?
- Which shared bridge should enforce policy for a later local stdio profile?

## Rejected Alternatives

### Root overlay manifest

A root manifest duplicates discovery roots, package identity, source metadata,
secret declarations, roles, and overlay precedence. Extending the current
per-package contract keeps one source of truth.

### Bespoke Python client per server

This remains appropriate for intentionally narrower domain APIs, but a generic
MCP path should not repeat session, SSE, JSON-RPC, and schema handling for every
integration.

### Harness overlays only

Harness overlays provide client configuration but not principal authorization,
managed egress, gateway-only credentials, protocol policy, or lifecycle
barriers.

### Static iron-proxy configuration only

Kubernetes sandboxes use managed proxy sync. A static-only implementation would
work in local unmanaged environments without implementing the production path.

### Direct upstream URLs

Direct harness URLs make policy and credential delivery optional client
behavior. Version 1 requires a generated route so iron-proxy remains the
enforcement boundary.

### Reusing the CLI role by default

A local CLI and a remote MCP surface can have different risk. Silently expanding
an existing `tool-<slug>` grant would make an overlay refresh an implicit
privilege increase, so MCP uses a separate role unless explicitly configured.
