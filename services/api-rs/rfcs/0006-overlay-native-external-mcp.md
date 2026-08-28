# RFC 0006: Policy-Enforced Overlay-Native External MCP Tools

Status: Draft
Owner: TBD
Target: `services/api-rs`, `services/sandbox`, `services/console`,
`services/iron-proxy`

## Summary

Let an overlay register a remote Model Context Protocol server by extending the
existing per-tool `pyproject.toml` contract. Centaur should discover one typed,
normalized declaration, publish it as versioned control-plane state, render the
authorized server into each supported harness, and compile the same declaration
into principal-scoped iron-proxy gateway, egress, credential, protocol, and tool
policy.

Version 1 is deliberately a **remote, tool-only Streamable HTTP profile**. It
allows the MCP lifecycle plus `tools/list` and explicitly authorized
`tools/call` requests. Resources, prompts, completion, sampling, roots,
elicitation, logging control, unknown methods, and local stdio servers remain
out of scope until Centaur can enforce them explicitly in both protocol
directions.

An operator should not need to:

- hand-write a different MCP config for every harness;
- copy a token into a sandbox;
- implement a bespoke Python JSON-RPC client for every MCP server; or
- add a second root-level overlay manifest that duplicates tool, secret, role,
  and source metadata Centaur already discovers.

The proposed unit remains a tool package directory, but its capabilities become
explicit. A directory may provide a CLI, one remote MCP server, or both. An
MCP-only directory does not synthesize a CLI or appear in Centaur's existing
public `/mcp` tool catalog. Skills remain in `.agents/skills/`, where the overlay
loader already supports them, and may use the same slug to document the
corresponding CLI and MCP capability.

This RFC covers Centaur acting as an MCP **client** inside agent sandboxes. It
does not replace or modify the existing `/mcp` endpoint, where Centaur acts as
an MCP server for external clients.

## Motivation

Centaur has native overlay delivery for tools, workflows, skills, personas, and
prompts. Tools already use `pyproject.toml` as a declarative boundary for CLI
scripts, secret references, allowed credential hosts, and per-tool labels.
There is no equivalent supported path for a sandbox to consume a third-party
MCP server.

The current escape hatch is harness-specific configuration such as
`CODEX_CONFIG_OVERLAY`. It can make a remote server reachable, but it is not a
sufficient product contract because it:

- configures one harness rather than all supported harnesses;
- is deployment-wide rather than a discovered overlay capability;
- does not create principal-scoped managed proxy policy;
- cannot safely bind a credential only to an MCP gateway route;
- cannot drive principal-specific discoverability;
- has no shared validation for URLs, transports, methods, tools, credentials,
  or route identity; and
- cannot participate in warm-pool assignment and revocation barriers.

The repository also contains MCP-specific clients for individual integrations.
Those clients prove demand but repeat transport, session, error, and response
parsing that a shared MCP implementation should own once.

Related work:

- [#1176](https://github.com/paradigmxyz/centaur/issues/1176) describes the
  external-MCP client gap and a working Codex-only configuration workaround.
- [#1385](https://github.com/paradigmxyz/centaur/issues/1385) explains why the
  static `iron-proxy.yaml` allowlist is not a Kubernetes managed-mode control
  surface.
- [#205](https://github.com/paradigmxyz/centaur/pull/205) identified protocol
  headers required by stateful Streamable HTTP sessions.
- [#883](https://github.com/paradigmxyz/centaur/pull/883) proposes role-based
  visibility for Centaur's MCP server direction. This RFC follows the same
  role-oriented control-plane pattern without depending on that PR being
  merged.
- [#841](https://github.com/paradigmxyz/centaur/pull/841) implements Centaur as
  an MCP server. This RFC addresses the opposite direction.

## Goals

- Declare a remote Streamable HTTP MCP server in an overlay tool directory.
- Make CLI and external MCP capabilities independently discoverable and
  grantable.
- Use one normalized declaration to configure every supported harness.
- Reuse existing secret source types and grant patterns without turning an MCP
  credential into a generic host-scoped sandbox credential.
- Keep real credentials out of sandbox files, environment variables, process
  arguments, and logs.
- Route every authorized MCP connection through a generated gateway route whose
  upstream egress and optional credential are route-only.
- Apply default-deny MCP method and tool policy at the iron-proxy boundary in
  both protocol directions.
- Make server visibility follow an explicit principal authorization formula.
- Preserve existing CLI, secret, skill, persona, and overlay behavior when no
  external MCP declaration is present.
- Publish source-attributed catalog generations that Console, iron-control,
  sandbox assignment, and diagnostics can reconcile deterministically.
- Make anonymous MCP servers possible without making them implicitly available
  to every principal.
- Fail a new external-MCP assignment before agent input when proxy and harness
  generations cannot be proven consistent.

## Non-Goals

- Replacing or changing the existing CLI tool, secret, skill, persona, or
  workflow formats.
- Changing existing `tool-<slug>` grants or existing generic HTTP secret
  behavior.
- Changing the behavior of sandboxes that have no authorized external MCP
  servers.
- Adding a root `centaur.integrations.toml` manifest.
- Supporting legacy HTTP+SSE transport in the first implementation.
- Supporting MCP resources, prompts, completion, sampling, roots, elicitation,
  or unrestricted server-to-client requests in version 1.
- Running arbitrary install or shell commands from overlay metadata.
- Solving general MCP OAuth discovery, browser consent, or dynamic client
  registration in the first implementation.
- Aggregating external MCP servers into Centaur's public `/mcp` endpoint.
- Treating harness configuration as an authorization boundary. iron-proxy and
  principal grants remain the enforcement boundary.
- Supporting local stdio MCP packages in the first implementation. See
  [Local stdio servers](#local-stdio-servers) for the follow-up shape.
- Retrofitting the new route-only MCP policy onto existing direct HTTP or CLI
  integrations.

## Prior Art

[add-mcp](https://github.com/neon-solutions/add-mcp) demonstrates the useful
client-side half of this design: normalize one MCP server declaration, then
write the native configuration expected by different agent clients. Centaur
needs the same renderer boundary plus principal grants, overlay shadowing,
managed egress, gateway-only credentials, catalog generations, and assignment
barriers.

[FastMCP's proxy provider](https://gofastmcp.com/servers/providers/proxy) can
bridge MCP transports without reimplementing the protocol in each integration.
It is a candidate for local stdio compatibility, while direct native harness
configuration remains the simpler path for remote Streamable HTTP servers.

[iron-proxy's MCP policy and gateway](https://github.com/paradigmxyz/iron-proxy#mcp-policy)
already provide useful enforcement primitives. Version 1 requires those
primitives, plus route-only upstream authorization, gateway-only credential
binding, and a tool-only method profile, to be available through Centaur's
managed control-plane path before the feature is enabled.

## Proposed Overlay Shape

An anonymous remote server needs only metadata and an explicit authorization
choice:

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

No `client.py` or `[project.scripts]` entry is required for an MCP-only
directory. The new discovery path classifies this package as MCP-only and does
not add a synthetic executable tool entry.

A directory that also exposes a CLI keeps the existing packaging contract. The
CLI and MCP server still receive independent capability identities:

```toml
[project]
name = "chain-tools"
description = "Chain CLI and remote MCP server"
version = "0.1.0"
requires-python = ">=3.11"
dependencies = ["chain-tools-cli==1.2.3"]

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
usage = "mcp-gateway"
mode = "inject"
inject_header = "Authorization"
inject_formatter = "Bearer {{ .Value }}"
hosts = ["mcp.chain-tools.example.com"]
```

`credential` references a same-directory secret declaration whose new
`usage = "mcp-gateway"` marker makes it eligible only for generated MCP gateway
bindings. It reuses the existing secret source and host-scoping schema, but it
is not emitted into the ordinary generic `secrets` or `transforms` sections.
Existing secret declarations without `usage = "mcp-gateway"` retain their
current behavior.

A package that intentionally needs the same underlying value for both a CLI and
MCP should declare two policy identities with distinct `name` values and the
same `secret_ref`: one existing generic declaration for the CLI and one
`mcp-gateway` declaration for the MCP route. This keeps the two grants and wire
behaviors independently auditable.

The exact array/table syntax should extend the existing tool parser rather than
introducing a second manifest or parser.

### MCP fields

| Field | Required | Meaning |
| --- | --- | --- |
| `name` | yes | Stable logical, harness, and audit name within the package. |
| `transport` | yes | `streamable-http` in version 1. |
| `url` | yes | HTTPS upstream URL with a DNS hostname and normalized path. |
| `allowed_tools` | yes | Explicit upstream tool names accepted by policy. |
| `credential` | no | Same-directory `mcp-gateway` secret name used only by the generated route. |
| `public` | no | Explicitly expose a credentialless server without a role; defaults to `false`. |
| `role` | no | Explicit role foreign ID; defaults to a generated MCP-specific role. |
| `startup_timeout_sec` | no | Harness-neutral connection timeout with bounded limits. |
| `tool_timeout_sec` | no | Harness-neutral call timeout with bounded limits. |
| `enabled_harnesses` | no | Narrowing-only list; omitted means all compatible harnesses. |

The URL is declarative because api-rs and Console must derive route identity,
egress, credential scope, and diagnostics without importing or executing
overlay code. URL templates, shell interpolation, fragments, literal
authorization headers, IP-literal hosts, and userinfo are rejected.

The MCP URL itself is the source of the upstream host. Existing
`[tool.centaur].hosts` remains available for existing CLI secret behavior but is
not required for an MCP-only package. A referenced `mcp-gateway` secret must be
scoped to the normalized upstream host and, where supported, the narrowest
practical path.

`allowed_tools` is deliberately explicit. Version 1 has no `allow_all_tools`
form and no argument-condition syntax. A later typed tool-table form may add
argument conditions after the pinned proxy contract and cross-parser behavior
are specified and tested. Centaur must never convert an absent or empty list
into allow-all.

`public = true` is accepted only for a credentialless server, remains subject to
an operator-level deployment policy, and is deliberately conspicuous. An
anonymous server may still perform consequential actions, so credential
presence is not an authorization predicate.

### Capability bundles and skills

No new bundle manifest is necessary. Existing overlay paths already describe
the useful surfaces:

```text
centaur-overlay/
├── tools/
│   └── ethereum/
│       ├── pyproject.toml       # CLI and/or remote MCP declaration
│       ├── client.py            # optional ordinary tool methods
│       └── ethereum_tool/       # optional CLI implementation
└── .agents/
    └── skills/
        └── ethereum/
            └── SKILL.md         # how and when to use the capability
```

Matching slugs are a convention, not a new ownership model. The skill remains
instructions and never grants MCP, CLI, network, or credential access.

## Capability Identity and Authorization

External MCP servers use a separate role by default:

```text
mcp-<package-slug>-<server-slug>
```

The generated role follows the existing role and grant patterns, but does not
silently expand an existing `tool-<slug>` CLI grant. An operator may set an
explicit existing role in trusted overlay metadata when CLI and MCP are
intentionally one indivisible capability.

For interactive sessions, the effective external MCP catalog is computed as:

1. the conversation/session principal must hold the MCP role; and
2. when a requester principal is present, the requester must also hold the MCP
   role.

Workflow, tool-host, and other service sessions with no requester use their
service principal alone. Future requester-hoisted or always-available MCP roles
require a separate explicit grant type; they are not inferred from credential
hoisting.

A principal role assignment grants the server capability, not a raw URL or
credential. When the server references a credential, the role also authorizes
the corresponding gateway-only binding. The compiler must verify that the
credential source is resolvable for the effective principal set before it emits
the route.

## Normalized Control-Plane Model

Discovery should produce explicit package capabilities rather than assuming
every `pyproject.toml` directory is executable:

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
into existing executable tool discovery, CLI shim installation, or Centaur's
existing public `/mcp` tool catalog.

### MCP-specific overlay shadowing

The new external MCP catalog uses the same ordered overlay inputs but has
fail-closed shadowing semantics for MCP declarations only:

- a later package directory claims its external MCP package identity before the
  declaration is fully validated;
- a valid later declaration replaces the earlier declaration;
- an invalid later declaration creates a visible tombstone and suppresses the
  earlier external MCP declaration; and
- existing CLI, persona, skill, and secret shadowing behavior remains unchanged.

Two active packages that normalize to the same logical harness or route name
without an overlay relationship are invalid. Logical names, DNS-safe route IDs,
and display names are distinct fields so slugification collisions cannot
silently merge capabilities.

Discovery fails or tombstones the individual external MCP declaration, with a
visible diagnostic, when:

- the URL is not HTTPS, lacks a DNS hostname, contains userinfo or a fragment,
  uses an IP literal, or uses an unsupported transport;
- the normalized upstream resolves to a private, loopback, link-local, or
  metadata address without a separate trusted internal-host policy;
- a referenced credential is absent, is not marked `mcp-gateway`, or is not
  scoped to the upstream host;
- `public = true` is combined with a credential;
- `allowed_tools` is empty, duplicated, or invalid;
- the generated or explicit role is invalid;
- two active entries collide after logical-name or route-ID normalization; or
- a timeout or harness name is unsupported.

The API should expose the normalized catalog and validation status to Console
and diagnostics. Ordinary repository discovery must not execute a server's
`tools/list` or otherwise contact the declared URL.

## Catalog Publication and Generations

api-rs owns parsing overlay TOML. Console and iron-control consume a normalized,
authenticated catalog generation and must not independently reparse the
manifest.

Each published generation contains:

- source repository, ref, commit, and configured overlay order;
- normalized entries and tombstones;
- generated role identities and credential references, never credential values;
- a canonical catalog digest; and
- parser and validation diagnostics.

Publication is an idempotent compare-and-swap keyed by the configured source
identity and commit. A deployment must designate one reconciler for a source so
multiple api-rs replicas cannot overwrite the same source with competing
commits. Removal publishes a tombstone or a newer generation; absence is not
interpreted as a successful empty catalog until the source generation is known.

Downstream state carries separate hashes:

1. `catalog_hash` for normalized overlay declarations;
2. `grant_hash` for the effective principal authorization set;
3. `proxy_config_hash` for the rendered managed proxy section; and
4. `harness_config_hash` for the final parsed harness configuration after
   operator overlays.

The hashes may be combined into an assignment generation for convenience, but
diagnostics retain the individual values so operators can distinguish catalog,
grant, proxy, and harness drift.

## Tool-Only MCP Protocol Profile

Version 1 exposes a fixed, default-deny protocol profile. The generated proxy
policy permits only:

- Streamable HTTP initialization and normal lifecycle messages;
- ping;
- cancellation and progress messages required by an authorized call;
- `tools/list`;
- `tools/call` for explicitly listed tool names; and
- session termination and resumability operations required by the pinned
  Streamable HTTP protocol version.

All other client-to-server and server-to-client JSON-RPC methods are denied.
This includes resources, prompts, completion, sampling, roots, elicitation,
logging control, and unknown extension methods. Server-originated requests and
notifications are policy-checked before reaching the harness; filtering only
`tools/list` is not treated as the authorization boundary.

The generated route also carries a route-specific protocol header and HTTP
method profile. It preserves the headers and methods required by the pinned
Streamable HTTP transport, including `MCP-Session-Id`,
`MCP-Protocol-Version`, `Last-Event-ID`, content negotiation, POST, GET, and
negotiated DELETE semantics. This is new route-local configuration and does not
require changing unrelated proxy traffic.

## Runtime Flow

1. api-rs scans ordered `TOOL_DIRS` and parses CLI, secret, and external MCP
   metadata into independent capability records.
2. MCP-specific overlay shadowing selects valid entries or fail-closed
   tombstones.
3. The designated reconciler publishes the normalized catalog generation to
   Console/iron-control.
4. The control plane computes the effective server set from the session and
   requester authorization formula.
5. Console/iron-control renders each granted server into the managed proxy
   assignment using route-only upstream egress, tool-only MCP policy, and an
   optional gateway-only credential binding.
6. A strict external-MCP assignment barrier confirms the proxy has applied the
   expected `proxy_config_hash` before the harness receives the server.
7. The sandbox renderer atomically writes the authorized MCP catalog into the
   selected harness's native format and reparses the final configuration after
   operator overlays.
8. The adapter hot-reloads or restarts the harness as required, then reports the
   applied `harness_config_hash`.
9. Only after both hashes match the assignment generation may Centaur deliver
   the first agent input.
10. The harness connects only to the generated client-facing route. iron-proxy
    applies the tool-only method profile, filters `tools/list`, denies unlisted
    calls, injects the route credential after authorization, and records
    protocol-aware audit metadata.

The strict barrier is new and applies only when an assignment includes external
MCP servers. Existing sandbox assignment behavior remains unchanged for
sessions without external MCP capabilities.

## Harness Rendering

Harness-specific syntax belongs in `services/sandbox`, not in overlay repos.
The renderer consumes one normalized JSON representation generated by the
control plane and writes:

- Codex `[mcp_servers.<name>]` TOML;
- Claude Code `mcpServers` JSON;
- Amp's MCP settings; and
- equivalent native configuration for other explicitly supported harnesses.

The client-facing URL must not use the special-use `.local` suffix described
by [RFC 6762](https://www.rfc-editor.org/rfc/rfc6762.html). It should use either
the existing per-sandbox proxy service with a generated route path or a
deployment-controlled internal DNS suffix. The logical MCP name remains
separate from the transport route ID.

The current `CODEX_CONFIG_OVERLAY` and `CLAUDE_SETTINGS_OVERLAY` remain operator
escape hatches and apply after generated configuration. An operator override
may narrow, rename, or remove generated harness entries, but it cannot widen
proxy policy or create a route that the effective assignment did not authorize.

The final merged configuration must be parsed again. Invalid output or a
reference to an unauthorized route is an assignment failure, not a warning
followed by a missing or partially configured server.

### Warm pools and refresh

Warm sandboxes make principal-specific MCP configuration a lifecycle concern.
Writing config only at image startup is insufficient because the principal is
not known until claim time, and many harnesses read MCP configuration only at
process startup.

Before enabling an adapter, it must implement one of:

- an authenticated hot-reload operation with an applied-generation response;
- a controlled harness restart after assignment but before first input; or
- a stable preconfigured local broker whose catalog is filtered after
  assignment and whose applied generation is queryable.

A warm-pool claim with external MCP is a transaction from the session's point
of view:

1. reserve the sandbox;
2. compute the effective assignment generation;
3. apply and verify proxy policy;
4. render and verify harness configuration;
5. expose the harness to agent input; or
6. mark the sandbox failed and remove it from the pool.

Repository additions do not become visible in a live session until an explicit
execution boundary or successful adapter reload. Removal and role revocation
fail closed: the proxy route is revoked first, active gateway sessions are
closed, new calls are denied, and harness visibility is removed afterward.
In-flight upstream requests may complete, but no new request may start under the
revoked generation.

## iron-proxy and iron-control

The pinned iron-proxy already has useful unmanaged concepts:

- `mcp.servers` for `tools/call` enforcement and filtered `tools/list`
  responses;
- `mcp_gateway.routes` for stable client-facing routes and upstream selection;
- ordinary egress policy; and
- existing secret source and transformation types.

External MCP requires a managed representation of those concepts plus three
new route-scoped guarantees:

1. **route-only upstream egress:** the upstream connection is authorized only
   when the request entered through the generated MCP route; a direct sandbox
   request to the upstream host or path does not inherit that authorization;
2. **gateway-only credential binding:** a referenced `mcp-gateway` secret is
   resolved and injected by the route after MCP authorization, and is never
   emitted as a generic host-scoped secret or transform; and
3. **tool-only bidirectional method policy:** both request and response streams
   are checked against the fixed version 1 profile.

Centaur must carry these fields through the managed iron-control and Console
snapshot path. Editing only `services/iron-proxy/iron-proxy.yaml` is not an
implementation because Kubernetes sandboxes use managed proxy sync.

For each granted MCP declaration, the managed proxy config contains:

1. a stable client-facing route using the per-sandbox proxy endpoint or a
   deployment-controlled internal suffix;
2. a route ID derived from package and MCP logical identity with collision
   validation;
3. an MCP server policy matching the route host and path;
4. the fixed version 1 method profile and explicit tool allowlist;
5. a route-only gateway mapping to the declared HTTPS upstream;
6. an optional gateway-only credential source binding;
7. the minimum upstream and token-endpoint egress required by that route; and
8. catalog, grant, and rendered policy hashes for audit and barriers.

The sandbox sees only the client-facing route and proxy CA. Neither the
generated harness config nor a shared adapter receives the real token.

### Policy ordering

The desired request boundary is:

```text
harness
  -> principal's iron-proxy
  -> generated MCP route match
  -> route-only egress authorization
  -> bidirectional MCP method profile
  -> tools/call allowlist
  -> gateway-only credential injection
  -> remote MCP server
```

The response boundary applies the corresponding method policy and filters
`tools/list` before content reaches the harness. Authorization is enforced on
every call; catalog filtering is not the security boundary.

### Credential handling

The MCP declaration names a gateway credential capability, not a value. The
same-directory `mcp-gateway` declaration reuses existing source kinds,
resolution, labels, and host scoping. The generated MCP role and effective
principal grants determine whether the route can resolve it.

The route is absent when a required credential is ungranted or unresolved. The
credential binding is never copied into sandbox environment variables, harness
files, process arguments, logs, or the ordinary generic proxy secret catalog.

Static bearer, brokered token, or existing OAuth-token source kinds may be used
when their lifecycle already fits a non-interactive route. General MCP OAuth
discovery, browser consent, audience negotiation, dynamic registration, and
per-user refresh ownership require a separate RFC.

## Compatibility and Unchanged Behavior

This proposal is additive. Implementations must prove the following invariants:

- a package without `[tool.centaur.mcp]` produces the same existing CLI, secret,
  persona, skill, and proxy behavior as before;
- an existing secret without `usage = "mcp-gateway"` keeps its existing
  registration and wire behavior;
- existing `tool-<slug>` roles do not gain external MCP capabilities by default;
- existing CLI and persona overlay shadowing is unchanged;
- the existing public `/mcp` endpoint and its tool catalog are unchanged;
- existing harness operator overlays remain available;
- existing proxy assignment and warm-pool behavior is unchanged when no
  external MCP server is assigned; and
- no static-only proxy configuration is introduced into the Kubernetes path.

## Local stdio Servers

Some packages expose a CLI, skills, and `--mcp` stdio mode from one artifact.
That is a useful later phase, but it has a different security boundary:
iron-proxy can constrain the child process's outbound HTTP, but it cannot
inspect the local stdio JSON-RPC stream.

A later declaration could look like:

```toml
[tool.centaur.mcp]
name = "ens"
transport = "stdio"
command = "ens"
args = ["--mcp"]
allowed_tools = ["get_address", "available", "price"]
```

The command must name an installed script from the same reviewed tool package;
absolute paths, shell strings, and arbitrary environment interpolation are
rejected. To preserve default-deny method and tool policy, Centaur would need
either:

- a shared local MCP bridge that filters both protocol directions before
  connecting the harness to the child process; or
- equivalent filtering implemented and tested in every harness adapter.

FastMCP's proxy provider is a candidate for a shared Python bridge because it
can proxy remote or local transports and present a local stdio server. It is an
implementation option, not part of the overlay schema. The official MCP SDK or
another maintained bridge can satisfy the same contract. Per-integration
Python files should not be required when a generated invocation of the shared
bridge is sufficient.

## Security Considerations

- External MCP descriptions, schemas, notifications, and tool results are
  untrusted input to the model.
- Tool and method names are policy identities from the upstream protocol, not
  display names rewritten by a harness.
- A server declaration alone never creates a role or credential grant for a
  principal.
- An anonymous server still requires its MCP-specific role unless explicitly
  marked `public = true` and permitted by deployment policy.
- URLs are validated without fetching them during discovery, avoiding
  control-plane SSRF.
- Route creation revalidates DNS and denies private, loopback, link-local, and
  metadata addresses unless a separate trusted internal-host policy applies.
- DNS is checked at connection time so validation cannot be bypassed by later
  resolution changes.
- The gateway route, not a broad host allowlist, owns upstream egress.
- Gateway credentials are scoped to exact hosts and the narrowest practical
  paths and are injected only after method and tool authorization.
- Direct upstream HTTP requests do not receive route egress or credentials.
- Redirects cannot escape the declared host and path policy or carry a
  credential to a different authority.
- Tool and method policy is default-deny. An absent, stale, or failed external
  MCP managed-policy sync makes the route unavailable.
- Operator harness overlays cannot create proxy authority.
- Catalog, role, and route removal revoke proxy authority before removing
  harness visibility.
- Audit logs may record source generation, principal set, server, MCP method,
  tool, decision, route, and policy version, but never authorization headers,
  likely-secret arguments, or response bodies by default.
- Overlay refresh is a policy change attributable to repository, ref, commit,
  overlay order, and catalog digest.

## Rollout Plan

### Phase 1: Typed discovery only

- Parse `[tool.centaur.mcp]` and `usage = "mcp-gateway"` into typed capability
  records.
- Classify MCP-only packages without adding them to existing executable or
  public MCP catalogs.
- Add MCP-specific fail-closed shadowing, tombstones, diagnostics, source
  attribution, and unit tests.
- Do not publish, render, or execute external MCP yet.

### Phase 2: Catalog publication and grants

- Add normalized catalog-generation storage and authenticated idempotent
  publication.
- Add MCP-specific role registration and the session/requester authorization
  formula.
- Add gateway-only credential bindings that reuse existing source kinds without
  entering the generic secret catalog.
- Expose catalog and grant hashes to diagnostics.

### Phase 3: Managed proxy enforcement

- Extend iron-control and Console proxy snapshots for routes, route-only egress,
  gateway-only credentials, tool-only bidirectional method policy, and
  route-local Streamable HTTP headers and methods.
- Require a pinned iron-proxy version that enforces those fields.
- Test direct upstream denial, redirects, DNS changes, anonymous servers, and
  credentialed servers through managed mode.

### Phase 4: Harness rendering and assignment barrier

- Render the normalized catalog for Codex first, then Claude Code and Amp.
- Add the strict external-MCP proxy-and-harness assignment generation barrier.
- Implement warm-pool claim failure and disposal when the barrier cannot be
  satisfied.
- Add controlled refresh and revocation behavior before enabling by default.

### Phase 5: Local stdio and package pass-through

- Add a controlled package/command declaration.
- Choose and pin a shared bidirectional protocol bridge.
- Enforce method and tool policy before the local child process receives a
  call.

These phases should be separate PRs. The first implementation is a cross-service
feature rather than a small overlay change: discovery is focused, while catalog
reconciliation, managed proxy enforcement, and warm-pool correctness are the
larger pieces.

## Validation Plan

- Parser fixtures for valid, shadowed, tombstoned, malformed, anonymous,
  public, credentialed, CLI-plus-MCP, and MCP-only declarations.
- Compatibility tests proving packages without new metadata and secrets without
  the new usage marker retain byte-equivalent normalized outputs.
- Tests proving MCP-only packages do not synthesize CLI entries or appear in
  Centaur's existing public `/mcp` catalog.
- Tests proving an invalid later MCP overlay suppresses an earlier MCP entry
  without changing the package's existing CLI shadowing behavior.
- Cross-parser tests proving api-rs and `centaur-perms` interpret
  `mcp-gateway` declarations, roles, sources, and hosts identically.
- Catalog publication tests for idempotency, competing replicas, source commit
  changes, tombstones, and removals.
- Authorization tests for conversation principals, requester principals,
  service principals, explicit roles, and `public = true`.
- Snapshot tests for Codex, Claude Code, and Amp renderers, including operator
  narrowing and unauthorized-route rejection.
- Managed-mode proxy tests for initialize, lifecycle notifications, ping,
  cancellation/progress, `tools/list`, `tools/call`, GET/SSE resumability,
  negotiated DELETE/session termination, and required protocol headers.
- Tests that resources, prompts, completion, sampling, roots, elicitation,
  logging control, unknown methods, and unauthorized server-originated messages
  are denied before reaching the other side.
- Tests that unlisted tools are filtered and denied before upstream receives
  them.
- Tests that direct requests to the upstream host or path receive neither route
  egress nor credentials, including raw HTTP, non-JSON bodies, redirects, and
  alternate DNS answers.
- Tests that a shared underlying `secret_ref` can back distinct generic and
  gateway-only declarations without merging their grants or wire behavior.
- Tests that an ungranted principal sees no server route and cannot use the
  generated gateway.
- Tests that the sandbox contains no real credential.
- Assignment-generation tests for cold create, warm-pool claim, role change,
  overlay refresh, proxy lag, harness reload failure, and sandbox disposal.
- Revocation tests proving the proxy route disappears first, active streams are
  closed, and new calls fail before harness visibility is removed.
- One real anonymous remote server and one controlled credentialed server in
  the local Kubernetes stack.

## Open Questions

- Should catalog publication be owned by a dedicated reconciler process or by a
  leader-elected api-rs replica?
- Should the client-facing route use the existing per-sandbox proxy service and
  path, or a deployment-controlled internal DNS suffix?
- Which harnesses can return an authenticated applied-generation result after
  hot reload, and which require a controlled restart?
- Should additions affect existing sessions only at execution boundaries, or
  may an adapter opt into immediate reload after proving the same assignment
  generation?
- Which existing secret source kinds are safe for a non-interactive
  `mcp-gateway` binding in version 1?
- Is general MCP OAuth best modeled as a new Console integration type or as an
  extension of brokered credentials?
- Should local stdio servers share the Python-based FastMCP proxy provider, the
  official SDK, or a small runtime-native bridge?

## Rejected Alternatives

### Root overlay manifest

A root manifest can list CLIs, MCP servers, and skills in one file, but it
duplicates discovery roots, secret metadata, role labels, shadowing, and package
identity. It also creates a non-operative file in existing overlays until every
control-plane consumer learns it. Extending the current per-tool contract keeps
one declarative source of truth.

### Bespoke Python client per MCP server

This works today and is appropriate when an integration intentionally exposes a
smaller domain API. It is wasteful as the generic MCP path: each client repeats
session initialization, JSON-RPC envelopes, SSE parsing, errors, schema
discovery, and protocol evolution.

### Harness overlays only

Harness overlays are useful escape hatches and have proven the transport can
work. They do not provide normalized validation, cross-harness portability,
principal visibility, managed egress, gateway-only credentials, or
protocol-aware proxy policy.

### Static iron-proxy configuration only

The static file is not consumed by Kubernetes managed-mode proxies. Supporting
only that path would create a local success case while leaving the production
control-plane path unimplemented.

### Direct upstream URLs in harness configuration

Pointing a harness directly at the upstream URL makes the route identity,
credential injection, and tool policy optional client behavior. Version 1
requires a generated gateway route so the proxy remains the enforcement
boundary.

### Reusing a generic HTTP secret as the MCP credential

A generic host-scoped secret can authorize traffic outside the MCP route. The
new `mcp-gateway` usage reuses source resolution and host scoping while keeping
the credential out of ordinary generic proxy transforms.

### Reusing the CLI role by default

A package's CLI and remote MCP surface may have different risk and lifecycle.
Silently expanding an existing `tool-<slug>` grant would make an overlay refresh
an implicit privilege escalation. External MCP therefore uses a separate role
unless trusted metadata explicitly selects an existing role.
