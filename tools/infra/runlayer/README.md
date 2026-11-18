# Personal Runlayer Plugin access

Sign into Console with Slack SSO, then connect your own Runlayer account in
**Integrations**. DMs and channel turns use the requester's connection; shared
accounts, role grants and email-based matching are unsupported.

```sh
runlayer discover
runlayer search "find recent build failures"
runlayer call EXACT_ADVERTISED_NAME '{"argument":"value"}'
```

Names and schemas come from discovery. Connector consent and approval responses
are returned unchanged. Calls are never automatically retried, and MCP sessions
and tool definitions last only for one CLI invocation.

## Setup

An administrator opens **Integrations** in Console, enters the tenant's exact
HTTPS Plugin MCP URL (ending in `/mcp`) on the Runlayer card, and connects.
The preset registers an OAuth client with the tenant and creates an ordinary
`custom` OAuth app with slug `runlayer`:

- `provider_config` holds the tenant's same-origin
  `/api/v1/oauth/{authorize,token,revoke}` endpoints and the MCP URL as
  `resource`; verify these against the tenant's OAuth metadata.
- `owner_only: true`: each user's connection is keyed by their Console user
  and serves only their own principals, never roles, other people, or email
  matches.
- `always_available: true`, so channel turns use the requester's connection.
- label `centaur-tool: runlayer`, copied onto each user's wrapper secret.

Nothing Runlayer-specific runs after setup. The flow uses S256 PKCE,
`client_secret_post`, and RFC 8707 resource binding; token-broker refreshes with
the same resource. Tokens stay encrypted in Console and iron-proxy injects them
only for the resource host and path. The CLI reads the MCP URL from Console's
sandbox OAuth-app catalog, so no deployment environment variables are needed.

Disconnect removes local access, then revokes both tokens at the tenant. If
revocation fails, Console says so; revoke the connection in Runlayer too.
Expired or dead credentials and disabled apps or users cannot supply tokens.

## Validation

Run the Python client tests, Console's `owner_only_connections` tests, and the proxy-sync database tests.
Verify two real users' distinct permissions, requester switching, disconnect and
connector consent through a sandbox before rollout.
