# RFC 0006: Overlay-Native External MCP Servers

Status: Draft
Owner: TBD
Target: `services/api-rs`, `services/sandbox`, `services/console`,
`services/iron-proxy`

## Summary

Let an overlay register a remote MCP server by extending the existing
per-tool `pyproject.toml` contract. Centaur should discover one normalized MCP
declaration, render it into each supported harness, and compile the same
declaration into principal-scoped iron-proxy egress, credential, gateway, and
tool policy.

An operator should not need to:

- hand-write a different MCP config for every harness;
- copy a token into a sandbox;
- implement a bespoke Python JSON-RPC client for every MCP server; or
- add a second root-level overlay manifest that duplicates tool, secret, and
  role metadata Centaur already discovers.

The proposed unit remains a tool directory. A directory may provide a CLI, a
remote MCP server, or both. Skills remain in `.agents/skills/`, where the
overlay loader already supports them, and may use the same slug to document the
corresponding CLI and MCP surface.

This RFC covers Centaur acting as an MCP **client** inside agent sandboxes. It
does not replace the existing `/mcp` endpoint, where Centaur acts as an MCP
server for external clients.

## Motivation

Centaur has native overlay delivery for tools, workflows, skills, personas,
and prompts. Tools already use `pyproject.toml` as a declarative boundary for
CLI scripts, secret references, allowed credential hosts, and per-tool labels.
There is no equivalent supported path for a sandbox to consume a third-party
MCP server.

The current escape hatch is harness-specific configuration such as
`CODEX_CONFIG_OVERLAY`. It can make a remote server work, but it has several
properties that make it unsuitable as the product contract:

- it only configures one harness;
- malformed configuration is ignored and fails as a missing server later;
- it is deployment-wide rather than a discovered overlay capability;
- it does not create a managed iron-proxy MCP policy or egress grant;
- it cannot drive principal-specific discoverability; and
- it has no shared validation for URLs, transports, credentials, or tool
  policy.

The repository also contains MCP-specific clients for individual integrations.
Those clients prove demand but repeat transport, session, error, and response
parsing that an MCP implementation should own once.

Related work:

- [#1176](https://github.com/paradigmxyz/centaur/issues/1176) describes the
  external-MCP client gap and a working Codex-only configuration workaround.
- [#1385](https://github.com/paradigmxyz/centaur/issues/1385) explains why the
  static `iron-proxy.yaml` allowlist is not a Kubernetes managed-mode control
  surface.
- [#205](https://github.com/paradigmxyz/centaur/pull/205) identified the MCP
  session and protocol headers required through iron-proxy.
- [#883](https://github.com/paradigmxyz/centaur/pull/883) proposes per-tool role
  filtering for Centaur's MCP server direction. External MCP visibility should
  use the same role vocabulary.
- [#841](https://github.com/paradigmxyz/centaur/pull/841) implements Centaur as
  an MCP server. This RFC addresses the opposite direction.

## Goals

- Declare a remote Streamable HTTP MCP server in an overlay tool directory.
- Use one normalized declaration to configure every supported harness.
- Reuse existing secret declarations and principal grants.
- Keep real credentials out of sandbox files, environment variables, process
  arguments, and logs.
- Apply default-deny MCP tool policy at the iron-proxy boundary.
- Make server visibility follow the principal's integration grant.
- Preserve ordered overlay shadowing and repo-cache refresh behavior.
- Validate the complete declaration before a sandbox receives it.
- Make anonymous MCP servers possible without making them implicitly public to
  every principal.

## Non-Goals

- Replacing the existing CLI tool or skill formats.
- Adding a root `centaur.integrations.toml` manifest.
- Supporting legacy HTTP+SSE transport in the first implementation.
- Running arbitrary install or shell commands from overlay metadata.
- Solving general MCP OAuth discovery in the first implementation.
- Aggregating external MCP servers into Centaur's public `/mcp` endpoint.
- Treating harness configuration as an authorization boundary. iron-proxy and
  principal grants remain the enforcement boundary.
- Supporting local stdio MCP packages in the first implementation. See
  [Local stdio servers](#local-stdio-servers) for the follow-up shape.

## Prior Art

[add-mcp](https://github.com/neon-solutions/add-mcp) demonstrates the useful
client-side half of this design: normalize one MCP server declaration, then
write the native configuration expected by different agent clients. Centaur
needs the same renderer boundary plus principal grants, overlay shadowing,
managed egress, and credential-safe proxy policy.

[FastMCP's proxy provider](https://gofastmcp.com/servers/providers/proxy) can
bridge MCP transports without reimplementing the protocol in each integration.
It is a candidate for local stdio compatibility, while direct native harness
configuration remains the simpler path for remote Streamable HTTP servers.

[iron-proxy's MCP policy and gateway](https://github.com/ironsh/iron-proxy#mcp-policy)
already define the enforcement primitives this proposal should compile to.
Centaur's missing piece is carrying those primitives through discovery,
iron-control managed mode, principal grants, and sandbox assignment.

## Proposed Overlay Shape

An anonymous remote server needs only metadata:

```toml
[project]
name = "ethereum-mcp"
description = "Ethereum developer tools exposed over MCP"
version = "0.1.0"
requires-python = ">=3.11"

[tool.centaur]
hosts = ["ethereum-mcp.example.com"]

[tool.centaur.mcp]
name = "ethereum"
transport = "streamable-http"
url = "https://ethereum-mcp.example.com/mcp"
allowed_tools = [
    "get_balance",
    "get_block",
    "get_transaction",
    "read_contract",
]
```

No `client.py` or `[project.scripts]` entry is required for an MCP-only
directory. A directory that also exposes a CLI keeps using the existing
packaging contract:

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
hosts = ["mcp.chain-tools.example.com"]

[tool.centaur.mcp]
name = "chain"
transport = "streamable-http"
url = "https://mcp.chain-tools.example.com/mcp"
allowed_tools = ["resolve_name", "get_balance"]
credential = "CHAIN_MCP_TOKEN"

[[tool.centaur.secrets]]
type = "http"
name = "CHAIN_MCP_TOKEN"
mode = "inject"
inject_header = "Authorization"
inject_formatter = "Bearer {{ .Value }}"
hosts = ["mcp.chain-tools.example.com"]
```

`credential` references a secret declared in the same directory. It never
contains a secret value. The exact array/table syntax should follow the
existing tool parser; the examples above illustrate the proposed semantic
contract rather than a second parser.

### MCP fields

| Field | Required | Meaning |
| --- | --- | --- |
| `name` | yes | Stable harness and audit name; unique after overlay shadowing. |
| `transport` | yes | `streamable-http` in v1. |
| `url` | yes | HTTPS upstream URL with a DNS hostname and normalized path. |
| `allowed_tools` | yes | Explicit remote tool names accepted by policy. |
| `credential` | no | Same-directory secret name used by the gateway. |
| `startup_timeout_sec` | no | Harness-neutral startup timeout with bounded limits. |
| `tool_timeout_sec` | no | Harness-neutral call timeout with bounded limits. |
| `enabled_harnesses` | no | Narrowing-only list; omitted means all compatible harnesses. |

The URL is declarative because api-rs and Console must derive egress and
credential rules without importing or executing overlay code. URL templates,
shell interpolation, and literal authorization headers are rejected.

`allowed_tools` is deliberately explicit. A future `allow_all_tools = true`
may be added as a conspicuous opt-in, but it must have a tested representation
in the pinned iron-proxy version. Centaur must not silently convert an absent
list into allow-all.

### Capability bundles and skills

No new bundle manifest is necessary. Existing overlay paths already describe
the three useful surfaces:

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

## Normalized Control-Plane Model

Tool discovery should produce a typed record in addition to the current CLI
catalog and secret proxy fragments:

```text
ExternalMcpServer {
  name
  package
  overlay
  transport
  upstream_url
  upstream_host
  allowed_tools
  credential_ref?
  required_role
  startup_timeout?
  tool_timeout?
  enabled_harnesses?
}
```

The default role is `tool-<slug>`, consistent with the role naming proposed in
#883. Unlike today's secretless CLI behavior, an anonymous external MCP server
is not automatically public. An operator must grant its role or explicitly
mark the declaration public. Remote tools can perform consequential actions
without needing a Centaur-managed credential, so secret presence is not an
adequate authorization predicate.

Discovery should fail the individual MCP declaration, with a visible
diagnostic, when:

- the URL is not HTTPS, lacks a DNS hostname, contains userinfo, or uses an
  unsupported transport;
- the URL host and declared host/credential policy disagree;
- a referenced credential is absent or not scoped to the upstream host;
- `allowed_tools` is empty, duplicated, or invalid;
- two active overlays produce the same MCP name without normal shadowing; or
- a timeout or harness name is unsupported.

The API should expose the normalized catalog and validation status to Console
and diagnostics. It should not execute a server's `tools/list` during ordinary
repository discovery.

## Runtime Flow

1. api-rs scans ordered `TOOL_DIRS` and parses CLI, secret, and MCP metadata.
2. Existing overlay shadowing selects one active declaration for each name.
3. The control plane intersects the catalog with the sandbox principal's
   effective roles.
4. Console/iron-control renders the granted servers into the proxy assignment.
5. The config barrier confirms the proxy has the matching egress, MCP, gateway,
   and credential policy before agent input runs.
6. The sandbox renderer atomically writes the granted MCP servers into the
   selected harness's native format.
7. The harness connects through the sandbox's existing iron-proxy route.
8. iron-proxy filters `tools/list`, denies unlisted `tools/call` requests,
   injects the real credential if one is granted, and records protocol-aware
   audit metadata.

Every downstream representation should carry a digest of the normalized MCP
catalog and principal grant set. Diagnostics can then distinguish repository
refresh, proxy sync, sandbox render, and harness load failures.

## Harness Rendering

Harness-specific syntax belongs in `services/sandbox`, not in overlay repos.
The renderer consumes one JSON representation generated by the control plane
and writes:

- Codex `[mcp_servers.<name>]` TOML;
- Claude Code `mcpServers` JSON;
- Amp's MCP settings; and
- equivalent native configuration for other supported harnesses.

The current `CODEX_CONFIG_OVERLAY` and `CLAUDE_SETTINGS_OVERLAY` remain operator
escape hatches and apply after generated configuration. An operator override
may narrow or replace generated settings, but it must not widen proxy policy.

Generated configuration must be parsed again after rendering. Invalid output
is a sandbox startup or assignment error, not a warning followed by a missing
server.

### Warm pools and refresh

Warm sandboxes make principal-specific MCP configuration a lifecycle concern.
Writing config only at image startup is insufficient because the principal is
not known until claim time, and many harnesses read MCP configuration only at
process startup.

Before implementation, each harness adapter must define one of:

- an authenticated hot-reload operation;
- a controlled harness restart after assignment but before first input; or
- a stable preconfigured local gateway whose catalog is filtered after
  assignment.

The config barrier must cover both proxy sync and harness visibility. Repo-cache
refresh during a live session should not expose a newly added server until the
principal policy and harness catalog advance to the same digest. Removal should
fail closed: revoke proxy policy first, then remove harness visibility.

## iron-proxy and iron-control

The pinned iron-proxy already has the relevant unmanaged configuration
concepts:

- `mcp.servers` for default-deny `tools/call` enforcement and filtered
  `tools/list` responses;
- `mcp_gateway.routes` for stable client-facing routes, upstream selection,
  and credential injection;
- the ordinary domain allowlist for egress; and
- the secrets transform for existing placeholder replacement.

Centaur does not yet propagate all of those concepts through managed mode. The
implementation must extend the iron-control model and Console snapshot rather
than editing only `services/iron-proxy/iron-proxy.yaml`. A static-file-only
change would work in unmanaged development and remain absent from Kubernetes.

For each granted MCP declaration, the managed proxy config should contain:

1. a stable client-facing route, preferably `<name>.mcp.local`, used by harness
   configuration;
2. an MCP server policy matching that host and path;
3. the explicit tool allowlist and any argument conditions;
4. a gateway route to the declared HTTPS upstream;
5. a reference to the principal-granted credential source, when required; and
6. the minimum egress policy needed for the route and any OAuth token endpoint.

The sandbox sees only the stable route and proxy CA. The proxy resolves the
credential through iron-control and injects it after policy accepts the MCP
request. Neither the generated harness config nor a shared Python adapter needs
the real token.

Centaur's base header allowlist must also preserve the protocol-defined
`Mcp-Session-Id` and `Mcp-Protocol-Version` headers. This is required even when
the initial handshake succeeds; stateful servers need the session header on
subsequent requests.

### Policy ordering

The desired request boundary is:

```text
harness
  -> principal's iron-proxy
  -> host/path egress policy
  -> MCP server match
  -> tools/call allowlist and argument policy
  -> gateway credential injection
  -> remote MCP server
```

The response boundary filters `tools/list` before the result reaches the
harness. Authorization is still enforced on `tools/call`; filtering is not the
security boundary.

### Credential handling

The MCP declaration names a credential capability, not a value. Existing
Console grants determine whether the current principal can resolve it. The
rendered route must be absent when a required credential is ungranted.

Static bearer injection can reuse the current HTTP secret model. Brokered or
OAuth credentials should reuse existing secret sources where their lifecycle
fits. General MCP OAuth discovery, browser consent, token audience handling,
and refresh ownership require a separate follow-up because they introduce
principal and session state beyond a static gateway route.

## Local stdio Servers

Some packages expose a CLI, skills, and `--mcp` stdio mode from one artifact.
That is a useful phase two, but it has a different security boundary:
iron-proxy can constrain the process's outbound HTTP, but it cannot inspect the
local stdio JSON-RPC stream.

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
rejected. To preserve default-deny tool policy, Centaur would need either:

- a shared local MCP bridge that filters the protocol before connecting the
  harness to the child process; or
- equivalent filtering implemented and tested in every harness adapter.

FastMCP's proxy provider is a candidate for a shared Python bridge because it
can proxy remote or local transports and present a local stdio server. It is an
implementation option, not part of the overlay schema. The official MCP SDK or
another maintained bridge can satisfy the same contract. Per-integration
Python files should not be required when a generated invocation of the shared
bridge is sufficient.

## Security Considerations

- External MCP descriptions and tool results are untrusted input to the model.
- Tool names must be policy identities from the upstream protocol, not display
  names rewritten by a harness.
- A server declaration alone never creates a credential grant.
- An anonymous server still requires an integration role unless explicitly
  public.
- URLs are validated without fetching them during discovery, avoiding SSRF from
  the control plane.
- iron-proxy must deny private, loopback, link-local, and metadata addresses
  after DNS resolution unless explicitly supported by a separate internal-host
  policy.
- Gateway credentials are scoped to exact hosts and the narrowest practical
  paths.
- Tool policy is default-deny. An absent or failed managed-policy sync makes the
  server unavailable.
- Audit logs may record server, MCP method, tool, decision, and policy version,
  but never authorization headers, arguments likely to contain secrets, or
  response bodies by default.
- Overlay refresh is a policy change and should be attributable to repo, ref,
  commit, and normalized catalog digest.

## Rollout Plan

### Phase 1: Typed discovery

- Parse `[tool.centaur.mcp]` into a normalized catalog.
- Add validation, overlay shadowing, diagnostics, and unit tests.
- Do not render it to sandboxes yet.

### Phase 2: Managed proxy policy

- Extend iron-control and Console snapshot models for MCP servers, gateway
  routes, and hostname egress policy.
- Preserve MCP protocol headers.
- Test anonymous and credentialed Streamable HTTP servers through managed mode.

### Phase 3: Harness rendering

- Render the normalized catalog for Codex first, then Claude Code and Amp.
- Define warm-pool claim and live-refresh behavior before enabling by default.
- Add end-to-end tests proving role changes update both visibility and policy.

### Phase 4: Local stdio and package pass-through

- Add a controlled package/command declaration.
- Choose and pin a shared protocol bridge.
- Enforce tool policy before the local child process receives a call.

These phases should be separate PRs. The first implementation is approximately
a medium cross-service feature rather than a small overlay change: discovery
and one harness are focused, while managed iron-control propagation and
warm-pool correctness are the larger pieces.

## Validation Plan

- Parser fixtures for valid, shadowed, malformed, anonymous, and credentialed
  MCP declarations.
- Cross-parser tests proving api-rs and `centaur-perms` interpret secrets and
  roles identically.
- Snapshot tests for Codex, Claude Code, and Amp renderers.
- Managed-mode proxy tests for `initialize`, `notifications/initialized`,
  `tools/list`, and `tools/call` with session headers.
- Tests that an unlisted tool is filtered and denied before upstream receives
  it.
- Tests that an ungranted principal sees no server route and cannot reach the
  upstream directly.
- Tests that the sandbox contains no real credential.
- Warm-pool tests for claim, role change, overlay refresh, and revocation.
- One real anonymous remote server and one controlled credentialed server in
  the local Kubernetes stack.

## Open Questions

- Should anonymous MCP integrations require `tool-<slug>` by default, or use a
  separate `mcp-<slug>` role?
- Does the pinned iron-proxy version have a safe explicit allow-all tool form,
  or should Centaur require concrete names until that contract exists?
- Should a stable gateway alias be mandatory, or may anonymous servers use
  their upstream URL directly?
- Which harnesses can hot-reload MCP configuration, and which require a restart
  on warm-pool claim?
- Should overlay refresh affect existing sessions immediately or only new
  executions after an explicit catalog generation change?
- Is general MCP OAuth best modeled as a new Console integration type or as an
  extension of brokered credentials?
- Should local stdio servers share the Python-based FastMCP proxy provider, the
  official SDK, or a small runtime-native bridge?

## Rejected Alternatives

### Root overlay manifest

A root manifest can list CLIs, MCP servers, and skills in one file, but it
duplicates discovery roots, secret metadata, role labels, shadowing, and
package identity. It also creates a non-operative file in existing overlays
until every control-plane consumer learns it. Extending the current per-tool
contract keeps one declarative source of truth.

### Bespoke Python client per MCP server

This works today and is appropriate when an integration intentionally exposes a
smaller domain API. It is wasteful as the generic MCP path: each client repeats
session initialization, JSON-RPC envelopes, SSE parsing, errors, schema
discovery, and protocol evolution.

### Harness overlays only

Harness overlays are useful escape hatches and have proven the transport can
work. They do not provide normalized validation, cross-harness portability,
principal visibility, managed egress, or protocol-aware proxy policy.

### Static iron-proxy configuration only

The static file is not consumed by Kubernetes managed-mode proxies. Supporting
only that path would create a local success case while leaving the production
control-plane path unimplemented.
