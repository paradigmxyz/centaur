# RFC 0004: Console OAuth for MCP Auth

Status: Draft
Owner: TBD
Target: `services/console`, `services/api-rs`

## Summary

Make Centaur's remote MCP endpoint use the MCP HTTP authorization flow, with
console acting as the OAuth authorization server and api-rs acting as the MCP
protected resource server.

The user should not need to copy a JWT from a console page into Amp. Instead:

1. A harness connects to `POST /mcp` without a token.
2. api-rs returns `401 Unauthorized` with a `WWW-Authenticate: Bearer ...`
   challenge that points at MCP Protected Resource Metadata.
3. The harness fetches the Protected Resource Metadata and learns that console
   is the authorization server.
4. The harness discovers console's OAuth metadata.
5. The harness registers as an OAuth client, or uses preconfigured client
   metadata.
6. The harness opens a browser to console's authorization endpoint with PKCE.
7. The user signs in with the normal console login/SSO flow.
8. Console ensures the signed-in user has an iron-control principal and returns
   an authorization code to the harness.
9. The harness exchanges the code for a bearer access token.
10. The harness calls `POST /mcp` with `Authorization: Bearer <access_token>`.
11. api-rs verifies the token and uses the encoded principal for MCP tool
    execution.

This matches the MCP authorization model used by HTTP-based MCP clients and
harnesses, while keeping console as the identity and permission UX.

## Motivation

The current MCP branch exposes the HTTP MCP transport and persistent tool
runners without a user-facing auth flow. That keeps the transport work small,
but MCP clients still need a standard way to sign in and bind tool execution to
the right console principal.

Remote MCP clients already know how to follow an OAuth-style authorization
flow. We should use that instead of asking users to paste bearer tokens.

Console already owns:

- user login and SSO
- user approval/disable state
- principals, roles, grants, and effective permissions
- the operator UI where users can understand what identity they are using

MCP auth should use that surface.

The important product property is that MCP permissions are controlled by the
principal. The access token should identify the principal. It should not copy
the principal's current grants into the token. If an operator changes the
principal's roles or grants, the next per-user tool runner/proxy sync should see
the updated permissions without reissuing the token.

## Goals

- Use MCP-standard HTTP authorization so Amp and other harnesses can start auth
  themselves.
- Keep the existing MCP endpoint as `POST /mcp`.
- Keep api-rs as the MCP protected resource server.
- Make console the OAuth authorization server for Centaur MCP.
- Use console login/SSO as the user authentication ceremony.
- Return bearer access tokens from a token endpoint, not from a copy-token page.
- Encode the iron-control principal id in the access token.
- Keep MCP authorization based on live principal grants in iron-control.
- Keep the current per-principal persistent MCP tool runner model.
- Support Dynamic Client Registration initially, since generic MCP clients may
  not be pre-registered with our console.

## Non-Goals

- Implement a local bridge.
- Make Slackbot the long-term issuer of MCP credentials.
- Put live credential grants or secret names inside access tokens.
- Build a full general-purpose OAuth provider for arbitrary third-party apps.
- Support every OAuth client authentication method in the first version.
- Require Tailscale identity for MCP auth.

## Current State

The MCP branch has:

- `POST /mcp` in api-rs.
- No MCP token issuer.
- An unauthenticated transport path used as the base for this authorization
  work.
- Persistent tool runners keyed by `principal_id`.

The persistent runner already wants the iron-control principal id. For a
proxied tool, api-rs creates or reuses a runner whose sandbox spec carries:

```text
iron_control_principal = <principal id>
CENTAUR_MCP_PRINCIPAL_ID = <principal id>
```

That means the access token should encode the iron-control principal id, not
only the console user id or email.

## Protocol Design

### Roles

Centaur maps MCP/OAuth roles as follows:

| Role | Centaur Component |
|------|-------------------|
| MCP protected resource server | api-rs `/mcp` |
| OAuth authorization server | console |
| OAuth client | Amp, Codex, VS Code, or another MCP harness/client |
| Resource owner | signed-in console user |

### Discovery Flow

When a harness calls `/mcp` without a valid token, api-rs returns:

```http
HTTP/1.1 401 Unauthorized
WWW-Authenticate: Bearer realm="mcp",
  resource_metadata="https://api.example.com/.well-known/oauth-protected-resource/mcp",
  scope="mcp:tools"
```

api-rs serves Protected Resource Metadata at both:

```text
/.well-known/oauth-protected-resource
/.well-known/oauth-protected-resource/mcp
```

Example:

```json
{
  "resource": "https://api.example.com/mcp",
  "authorization_servers": ["https://console.example.com"],
  "scopes_supported": ["mcp:tools"]
}
```

The `resource` value must be the canonical externally visible MCP endpoint.
For local preview dogfooding this can be `http://localhost:3000/mcp`; for
production it should be the public HTTPS MCP URL.

The harness then fetches authorization server metadata from console.

Console should serve OAuth Authorization Server Metadata at:

```text
/.well-known/oauth-authorization-server
```

Optionally, console can also serve:

```text
/.well-known/openid-configuration
```

Example metadata:

```json
{
  "issuer": "https://console.example.com",
  "authorization_endpoint": "https://console.example.com/mcp/oauth/authorize",
  "token_endpoint": "https://console.example.com/mcp/oauth/token",
  "registration_endpoint": "https://console.example.com/mcp/oauth/register",
  "response_types_supported": ["code"],
  "grant_types_supported": ["authorization_code", "refresh_token"],
  "code_challenge_methods_supported": ["S256"],
  "scopes_supported": ["mcp:tools"],
  "token_endpoint_auth_methods_supported": ["none"],
  "resource_indicators_supported": true
}
```

### Client Registration

First version should support Dynamic Client Registration because generic MCP
harnesses may not have a pre-registered Centaur client id.

Console endpoint:

```text
POST /mcp/oauth/register
```

Allowed registration shape:

```json
{
  "client_name": "Amp",
  "redirect_uris": ["http://127.0.0.1:49152/callback"],
  "grant_types": ["authorization_code", "refresh_token"],
  "response_types": ["code"],
  "token_endpoint_auth_method": "none"
}
```

Console returns:

```json
{
  "client_id": "mcp_client_...",
  "client_name": "Amp",
  "redirect_uris": ["http://127.0.0.1:49152/callback"],
  "grant_types": ["authorization_code", "refresh_token"],
  "response_types": ["code"],
  "token_endpoint_auth_method": "none",
  "client_id_issued_at": 1782749000
}
```

Registration constraints:

- public clients only in v1 (`token_endpoint_auth_method = "none"`)
- require PKCE S256 at authorize/token time
- allow loopback redirect URIs (`http://127.0.0.1`, `http://localhost`, `[::1]`)
- allow HTTPS redirect URIs
- reject wildcard redirect URIs
- reject non-loopback plain HTTP redirect URIs
- store only client metadata and timestamps, not user authorization

Future versions can add OAuth Client ID Metadata Documents. Do not advertise
`client_id_metadata_document_supported` until console actually validates those
documents.

### Authorization Endpoint

Console endpoint:

```text
GET /mcp/oauth/authorize
```

Required parameters:

```text
response_type=code
client_id=<registered client id>
redirect_uri=<one registered redirect URI>
code_challenge=<PKCE S256 challenge>
code_challenge_method=S256
resource=<canonical MCP resource URI>
scope=mcp:tools
state=<client state>
```

Behavior:

- If signed out, redirect through existing console login/SSO and return to the
  authorize request.
- If the console user is pending or disabled, deny authorization.
- Validate `client_id`, `redirect_uri`, `scope`, `resource`, and PKCE params.
- Ensure the signed-in user has an MCP principal.
- Optionally show a compact consent/confirmation page.
- Create a short-lived one-time authorization code.
- Redirect to the client `redirect_uri` with `code` and original `state`.

The authorization code stores:

- `client_id`
- `redirect_uri`
- `code_challenge`
- `code_challenge_method`
- `resource`
- `scope`
- `user_id`
- `principal_id`
- expiration timestamp, suggested 5 minutes
- consumed timestamp, initially null

### Token Endpoint

Console endpoint:

```text
POST /mcp/oauth/token
```

Authorization code exchange request:

```text
grant_type=authorization_code
code=<authorization code>
redirect_uri=<same redirect URI>
client_id=<client id>
code_verifier=<PKCE verifier>
resource=<canonical MCP resource URI>
```

Console validates:

- code exists, is unexpired, and is unused
- code belongs to `client_id`
- `redirect_uri` matches the code
- `resource` matches the code
- PKCE verifier matches the stored S256 challenge
- user is still active
- principal still exists

Console returns:

```json
{
  "access_token": "<jwt>",
  "token_type": "Bearer",
  "expires_in": 3600,
  "refresh_token": "cmcpr_...",
  "scope": "mcp:tools"
}
```

The access token is a JWT signed by console with the shared Centaur signing
secret. Refresh tokens are opaque random strings stored hashed by console.

Refresh token request:

```text
grant_type=refresh_token
refresh_token=<opaque refresh token>
client_id=<client id>
resource=<canonical MCP resource URI>
scope=mcp:tools
```

Refresh behavior:

- validate refresh token hash, client id, user status, principal, resource, and
  scope
- rotate refresh token on each use
- return a fresh access token

### Access Token Claims

Example JWT payload:

```json
{
  "iss": "https://console.example.com",
  "aud": "https://api.example.com/mcp",
  "sub": "usr_abc123",
  "jti": "mcp_at_018f...",
  "iat": 1782749000,
  "nbf": 1782749000,
  "exp": 1782752600,
  "scope": "mcp:tools",
  "client_id": "mcp_client_abc123",
  "principal_id": "prn_abc123",
  "principal_foreign_id": "console-user-alice-example-com",
  "principal_namespace": "default",
  "email": "alice@example.com",
  "name": "Alice Example"
}
```

Claim semantics:

| Claim | Purpose |
|-------|---------|
| `iss` | Console issuer URL from authorization server metadata. |
| `aud` | Canonical MCP resource URI from the authorization request. |
| `sub` | Console user oid. Useful for audit/debugging. |
| `jti` | Access token instance id. Used as `token_id` in MCP whoami/logs. |
| `iat`/`nbf`/`exp` | Token lifetime. |
| `scope` | MCP protocol scopes, not credential grants. |
| `client_id` | Registered OAuth client id. |
| `principal_id` | iron-control principal oid used for tool runner/proxy binding. |
| `principal_foreign_id` | Human/debug identifier. Not authoritative for proxy binding. |
| `principal_namespace` | Human/debug namespace. |
| `email`/`name` | Display only. |

The JWT must not include:

- secret ids
- role ids
- grant details
- actual credential values
- provider refresh tokens

### Principal Model

Console creates or finds one MCP principal per active console user.

Default mapping:

```text
namespace:  <configured namespace, default "default">
foreign_id: console-user-<slugified email>
name:       Console User <email>
labels:
  managed-by: centaur
  principal-kind: console-user
  console-user-id: <usr_...>
  email: <email>
```

The access token includes the resulting principal oid, for example `prn_...`.

The oid is what api-rs uses to bind the per-sandbox iron-proxy. The foreign id
and email are included for diagnostics only.

This gives us the permissions flexibility we want:

- grant tools/secrets directly to the user's console principal
- assign roles to the user's console principal
- later change grants/roles without changing MCP tokens
- later support group/team based assignment through console policy without
  changing MCP transport

### Signing Secret

Add one general Centaur JWT signing secret instead of an MCP-specific secret:

```text
CENTAUR_JWT_SIGNING_SECRET
```

This should live in the shared infra Secret and be mounted into both console and
api-rs.

Why not reuse Rails `SECRET_KEY_BASE`?

- It is tied to Rails cookies and framework internals.
- Rotating it has Rails-specific blast radius.
- api-rs should not need to treat Rails session signing material as an API auth
  root.
- A general Centaur JWT secret can support other future service-issued JWTs with
  issuer/audience separation.

The first version can use HS256 with this shared secret.

JWT verification in api-rs must require:

- known algorithm: `HS256`
- trusted issuer matching console metadata
- audience matching the canonical MCP resource URI
- `exp` in the future
- `nbf` absent or not in the future
- `iat` not unreasonably in the future
- required `principal_id`
- required `scope` containing `mcp:tools` or `mcp:*`

Future rotation can add:

```text
CENTAUR_JWT_SIGNING_KID
CENTAUR_JWT_VERIFYING_SECRETS
```

where the verifying env var is a JSON map of `kid -> secret`.

### MCP Endpoint Auth

api-rs should accept only console-issued OAuth access JWTs.

```text
Authorization: Bearer <value>

verify as console OAuth MCP access token
```

The verified identity should normalize into the existing MCP principal shape:

```text
McpAuthenticatedPrincipal {
  token_id: <jti>
  principal_id: <iron-control principal oid>
  name: <principal or user display name>
  scopes: ["mcp:tools"]
  expires_at: <jwt exp>
}
```

Everything after auth should stay the same:

- `tools/list` checks `mcp:tools`.
- `tools/call` checks `mcp:tools`.
- proxied tools use persistent runner keyed by `principal_id`.
- the runner sandbox uses that same `principal_id` for iron-proxy.

## Deployment Config

Add the shared secret to the infra Secret:

```text
CENTAUR_JWT_SIGNING_SECRET=<random 32+ bytes, base64 or hex encoded>
```

`just bootstrap-secrets` should generate it if absent and never rotate it in
place.

Chart wiring:

- api-rs already imports the shared infra Secret with `envFrom`, so it can read
  `CENTAUR_JWT_SIGNING_SECRET`.
- console should explicitly mount `CENTAUR_JWT_SIGNING_SECRET` from the shared
  infra Secret, because console intentionally does not use `envFrom`.
- api-rs needs a canonical MCP public URL for Protected Resource Metadata.
- console needs the same canonical MCP public URL for OAuth resource validation.
- console needs its own public issuer URL.

Suggested env vars:

```text
CENTAUR_JWT_SIGNING_SECRET
CENTAUR_MCP_PUBLIC_URL
CENTAUR_CONSOLE_PUBLIC_URL
CENTAUR_MCP_ACCESS_TOKEN_TTL_SECONDS
CENTAUR_MCP_REFRESH_TOKEN_TTL_SECONDS
CENTAUR_MCP_PRINCIPAL_NAMESPACE
```

`CENTAUR_JWT_SIGNING_SECRET` is intentionally general. The other env vars are
MCP-specific policy/display knobs.

## Security Considerations

### Bearer Token Risk

The OAuth access token is a bearer token. Anyone who obtains it can use the
encoded principal's MCP permissions until it expires.

Mitigations:

- short access token TTL, suggested 1 hour
- refresh tokens stored hashed by console
- refresh token rotation
- no access token values in logs
- no access token persistence in console DB
- no token values in Slack messages
- no grants embedded in access tokens
- future `jti` denylist if needed

### Disabled Users

Console checks user status when authorizing and refreshing.

Because access tokens are stateless, disabling a user does not automatically
invalidate already-issued access tokens until they expire. Short access token
TTL limits this window. Refresh tokens must stop working immediately for
disabled users.

### Permission Changes

Permission changes should not require new access tokens.

The token identifies `principal_id`; iron-control remains the live source of
truth for grants. If a role is revoked from the principal, the next proxy sync
should remove that credential from the user's runner.

### Audience and Resource Binding

The API must require `aud` to match the canonical MCP resource URI. Console must
issue access tokens only for the `resource` value supplied by the client and
accepted by console policy.

Do not accept generic audiences like `api` or `centaur`.

### DCR Abuse

Unauthenticated Dynamic Client Registration can be abused if unconstrained.

Initial constraints:

- only public clients
- loopback or HTTPS redirect URIs only
- no wildcard redirects
- no custom schemes initially
- rate limit registration
- audit client registrations
- optionally prune unused clients

### Secret Rotation

Initial implementation can use one shared signing secret.

Before production reliance, add a `kid` strategy:

- console signs with active `kid`
- api-rs verifies against active plus previous keys
- old keys stay in verify-only mode until all access tokens signed with them
  expire

## Alternatives Considered

### Manual Copyable JWT Page

Pros:

- simplest to implement
- no OAuth client registration, auth codes, or token endpoint

Cons:

- not the MCP-supported HTTP auth flow harnesses are built around
- poor UX for Amp and other clients
- users manually handle long bearer secrets
- harder to refresh tokens cleanly

This RFC replaces the copy-token page with MCP OAuth.

### Keep Opaque DB Tokens

Pros:

- simple revocation
- simple for a Slack-first prototype

Cons:

- api-rs remains an issuer
- Slackbot remains an issuance UX
- every non-Slack surface needs another token flow
- console login/SSO is not the source of user identity
- not the harness-native MCP auth path

### External OAuth Provider Only

We could point the MCP Protected Resource Metadata directly at Okta, Google, or
another IdP.

Pros:

- mature OAuth implementation
- less auth code in console

Cons:

- the access token still needs Centaur principal claims
- we still need a principal mapping layer
- group/role policy becomes split between IdP and iron-control
- local/dev and preview flows are harder

Console can still federate login to Okta/Google while issuing the Centaur MCP
access token itself.

### Tailscale MCP Auth

Pros:

- strong device/user identity on a tailnet

Cons:

- not every user is on the same tailnet
- does not solve Discord or external users
- still need to map tailnet identity to iron-control principals

### Reuse Rails `SECRET_KEY_BASE`

Pros:

- already exists
- console and api-rs can be wired to read it

Cons:

- wrong blast radius
- tied to Rails cookies/framework behavior
- not obviously safe to expose as a general API signing root

Use `CENTAUR_JWT_SIGNING_SECRET` instead.

## Rollout Plan

1. Add this RFC.
2. Add `CENTAUR_JWT_SIGNING_SECRET` bootstrap and chart wiring.
3. Add api-rs Protected Resource Metadata with console authorization server URL.
4. Add console OAuth authorization server metadata.
5. Add console Dynamic Client Registration.
6. Add console authorization code + PKCE flow.
7. Add console token endpoint with JWT access tokens and opaque refresh tokens.
8. Add console principal resolution for signed-in users.
9. Add api-rs JWT access token verification for MCP bearer auth.
10. Replace the unauthenticated MCP path with JWT bearer verification.
11. Dogfood with Amp using a local port-forwarded preview API.

## Test Plan

api-rs tests:

- missing bearer returns `401` with `WWW-Authenticate` containing
  `resource_metadata` and `scope="mcp:tools"`
- Protected Resource Metadata returns the configured resource and console
  authorization server
- expired JWT is rejected
- wrong issuer is rejected
- wrong audience/resource is rejected
- missing `principal_id` is rejected
- missing `mcp:tools` scope is rejected
- valid JWT authenticates and `centaur_whoami` reports principal and `jti`

Console tests:

- authorization metadata includes required endpoints and supported capabilities
- DCR accepts loopback redirect URIs
- DCR rejects wildcard and non-loopback HTTP redirect URIs
- signed-out authorize request redirects to login and resumes
- pending user cannot authorize
- disabled user cannot authorize
- active user can authorize
- authorization code is one-time-use
- wrong PKCE verifier is rejected
- token endpoint returns bearer JWT with expected issuer, audience, subject,
  principal, scope, and expiration
- refresh token is stored hashed and rotates on use
- disabled users cannot refresh
- raw signing secret is never rendered

Chart/script tests:

- `just bootstrap-secrets` creates `CENTAUR_JWT_SIGNING_SECRET` only when absent
- console deployment receives `CENTAUR_JWT_SIGNING_SECRET`
- api-rs receives the same secret
- Helm lint/template pass

Manual dogfood:

- port-forward preview api-rs
- configure Amp with the preview MCP URL
- verify Amp opens browser auth automatically
- sign in through console
- complete PKCE code exchange
- call `centaur_whoami`
- call a non-secret tool
- call a proxied tool granted to the console principal
- revoke a grant and verify subsequent proxied calls lose access

## Open Questions

- Do we need refresh tokens in v1, or will access-token-only be acceptable for
  the harnesses we care about?
- Should we support OAuth Client ID Metadata Documents in v1, or is DCR enough
  for Amp/Codex/VS Code?
- Default access token TTL: 1 hour, 8 hours, or environment-specific?
- Default refresh token TTL: 7 days, 30 days, or environment-specific?
- Should the console principal namespace default to `default` or a dedicated
  namespace like `mcp`?
- Do we want a first-version access-token `jti` denylist, or is short TTL
  enough?
