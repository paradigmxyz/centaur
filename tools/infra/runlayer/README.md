# Personal Runlayer Plugin access

Connect Runlayer once in Console **Integrations**. Each person connects their own
account. Centaur always uses the requester's connection in DMs and channels;
shared accounts and role-based access are unsupported. There is no
**always available** toggle for Runlayer. Sign into Console with Slack SSO to link
the verified Slack user and team; email/profile text is not used to select a
Runlayer connection.

```sh
runlayer discover
runlayer search "find recent build failures"
runlayer call EXACT_ADVERTISED_NAME '{"argument":"value"}'
```

Discovery and execution use one unified Runlayer Plugin MCP connection. Exact
names and schemas come from Runlayer; there are no per-service adapters. The
client supports paginated tools/list, JSON and SSE replies, flattened tools,
and the nested search_tools/execute_tool presentation. Connector consent and
approval responses remain visible. Calls are never automatically replayed.
MCP sessions and tool definitions are not persisted between CLI invocations.

## Deployment

Set the non-secret environment variable `RUNLAYER_MCP_URL` to the exact HTTPS
unified Plugin MCP URL supplied by Runlayer. Supply the same value to Console
web, Console workers, api-rs, proxy-sync, and sandboxes. The URL is deployment
configuration; it is not entered in Console or stored on an OAuth App.

With Helm, use the existing `sandbox.extraEnv` setting and the generic
`console.extraEnv`, `apiRs.extraEnv`, and `proxySync.extraEnv` maps (Console's map applies to both
web and worker processes):

```yaml
console:
  extraEnv:
    RUNLAYER_MCP_URL: https://your-tenant.runlayer.com/mcp
apiRs:
  extraEnv:
    RUNLAYER_MCP_URL: https://your-tenant.runlayer.com/mcp
proxySync:
  extraEnv:
    RUNLAYER_MCP_URL: https://your-tenant.runlayer.com/mcp
sandbox:
  extraEnv:
    RUNLAYER_MCP_URL: https://your-tenant.runlayer.com/mcp
```

There is no dedicated Runlayer chart value. Without a configured URL, Console
hides the registration preset and proxy-sync delivers no Runlayer credentials.

The supported authorization server endpoints under the same origin are
`/api/v1/oauth/{authorize,token,register,revoke}`. The resource scope is
`mcp:proxy`. Confirm these against the tenant's public OAuth metadata before
rollout. The implementation uses authorization code + S256 PKCE with
`client_secret_post`, and sends the resource on authorization, code exchange,
and refresh. It does not require an agent account or OBO delegation.

After setting the environment, an administrator clicks Connect on the Runlayer
preset in Integrations to register the OAuth client through DCR. Alternatively
create a personal OAuth App manually with provider `runlayer`, the registered
client ID/secret, and scope `mcp:proxy`; register
`<Console public URL>/oauth/<app-slug>/callback` exactly. `shared` must be false;
Centaur fixes `always_available` to true so channel turns use the requester's
personal connection. These choices are hidden in the form and enforced by the
model, including for API changes. Every person then connects their own Runlayer
account through that app.

The gateway reads its endpoint from `RUNLAYER_MCP_URL` and its consent link from
Console's authenticated sandbox OAuth-app catalog. Tokens are encrypted in the
existing credential broker and injected by iron-proxy only on GET/POST/DELETE
to the configured host and path. The gateway carries no shared token, user ID
argument, client secret, refresh token, or real access token. The exact resource
URL determines the credential's request rules; no wildcard host grant is used.
Centaur's existing sandbox and proxy network policies apply.

A connection is keyed by its Centaur owner. MCP OAuth does not require an
OIDC/userinfo response, so Centaur does not decode an opaque access token or
claim that its local connection key is a Runlayer account ID. Reconnect replaces
that owner's grant. The Runlayer login/consent screen selects the external
account, whose access Runlayer enforces on discovery and execution.

Disconnect removes local access and requests access- and refresh-token revocation from
Runlayer. A failed remote revocation is reported, while local access remains
disabled. Dead/expired grants, disabled apps/users, role grants and grants to a
different person cannot supply a personal Runlayer token. Changing the configured tenant invalidates existing connections; register a
client for the new tenant and reconnect each person. Missing connector authorization remains a separate
Runlayer consent step and is surfaced through its response/link.

Runlayer refreshes currently run in Console, including resource binding and
configuration-change checks. Keep `tokenBroker.enabled: false` when using
Runlayer; the Rust token-broker does not yet support this provider and enabling
it disables Console's refresh jobs.

## Validation

Run the Python package tests and the Console personal_runlayer integration
tests, plus the proxy-sync database integration suite. The latter exercises
A → B → A requester changes, shared/cross-owner grant rejection, revocation and
expiry against the same database schema used by Console. Before rollout, two
real users must complete Runlayer consent and verify different discovery and
execution permissions, revocation, and connector-specific consent.
