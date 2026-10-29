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

Set `RUNLAYER_MCP_URL` to the exact HTTPS Plugin MCP URL supplied by Runlayer.
Use the same value in Helm's `console.extraEnv` (web and workers), `apiRs.extraEnv`,
`proxySync.extraEnv` and `sandbox.extraEnv` maps. Keep `tokenBroker.enabled: false`:
Runlayer refresh and resource checks currently require Console.

An administrator connects the Runlayer preset in Integrations to register the
OAuth client. Alternatively, create a personal `runlayer` OAuth App with its
registered client ID/secret, scope `mcp:proxy`, and callback
`<Console public URL>/oauth/<app-slug>/callback`. Availability in channels is automatic.

The provider uses S256 PKCE, `client_secret_post`, resource binding, and same-origin
`/api/v1/oauth/{authorize,token,register,revoke}` endpoints; verify these against
the tenant's OAuth metadata. Tokens remain encrypted in Console and are injected
by iron-proxy only for GET/POST/DELETE to the configured host and path.

Disconnect removes local access even if remote revocation fails; follow the
reported Runlayer revocation step in that case. Expired/dead credentials and
disabled apps/users cannot supply tokens. After changing tenants, register a new
client and reconnect each user. Missing configuration withholds credentials.

## Validation

Run the Python client, Console `personal_runlayer`, and proxy-sync database tests.
Verify two real users' distinct permissions, requester switching, disconnect and
connector consent through a sandbox before rollout.
