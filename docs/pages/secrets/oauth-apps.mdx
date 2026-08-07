---
title: OAuth Apps
description: Register OAuth clients, collect user consent, and grant refreshed access tokens to Centaur principals.
---

# OAuth Apps

OAuth apps let users connect their own upstream accounts to Centaur. An operator
registers an OAuth client in the console, shares a consent link, and each user
who completes the flow gets a managed credential. The Centaur Console keeps the
token fresh and iron-proxy injects it as `Authorization: Bearer <access token>`
into requests to the provider's API hosts. Refresh tokens never leave the
Centaur Console.

## Supported Providers

| Provider | Use |
|----------|-----|
| `google` | Google APIs, such as Gmail or Drive scopes. |
| `slack` | Slack user tokens with normal Slack API scopes. |
| `github` | GitHub user tokens for `api.github.com`. |
| `granola` | Granola MCP tokens for `mcp.granola.ai`. |
| `linear` | Linear tokens for `api.linear.app`. |
| `attio` | Attio workspace tokens for `api.attio.com`. |
| `hubspot` | HubSpot private-app OAuth tokens for `api.hubapi.com`. |
| `microsoft` | Microsoft Graph user tokens for `graph.microsoft.com`. |

## Set Up An App

1. **Create an OAuth client with the provider** (for example in the Google
   Cloud console or the Attio developer dashboard). Register this callback
   URL: `<CENTAUR_CONSOLE_PUBLIC_URL>/oauth/<slug>/callback`.
2. **Register it in Centaur.** In the console, open **OAuth Apps**, click
   **Add App**, and fill in the slug, provider, client id, client
   secret, and allowed scopes (one per line).
3. **Share the consent link** shown on the app page:
   `<CENTAUR_CONSOLE_PUBLIC_URL>/oauth/<slug>/start`. Each user who opens it
   and approves the provider's consent screen gets a credential, wrapped in a
   grantable secret.

Re-consenting with the same account updates the existing credential instead of
creating another one.

## Provider-Specific Setup

### Granola

Granola has no app dashboard; obtain the OAuth client once via dynamic client
registration, then use the returned `client_id` and `client_secret` when adding
the app in the console:

```bash
curl -sS -X POST https://mcp-auth.granola.ai/oauth2/register \
  -H "Content-Type: application/json" \
  -d '{
    "client_name": "Centaur Console",
    "redirect_uris": ["<CENTAUR_CONSOLE_PUBLIC_URL>/oauth/granola/callback"],
    "grant_types": ["authorization_code", "refresh_token"],
    "response_types": ["code"],
    "token_endpoint_auth_method": "client_secret_post",
    "scope": "openid email profile offline_access mcp"
  }'
```

Use `mcp` as the allowed scope for the app.

### HubSpot

Create a HubSpot app with OAuth enabled and register the Centaur callback URL.
Include every required scope in both the HubSpot app Auth settings and the
console app's allowed scopes (HubSpot treats dashboard-checked scopes as
required on the authorize URL). The `oauth` scope is always present. After
consent, Centaur enriches the credential from HubSpot's access-token metadata
endpoint so the subject is `{hub_id}:{user_id}`.

### Microsoft

Register an Entra ID app registration that allows the authorization-code flow
with a client secret, and add the Centaur callback URL. Request Microsoft Graph
delegated permissions as allowed scopes (for example `User.Read`, `Mail.Read`).
Centaur always adds `openid profile email offline_access` so the token response
carries an id_token and a refresh token. The strategy uses the `common` tenant
endpoints; single-tenant apps still work when the id_token issuer is
`https://login.microsoftonline.com/<tenant-id>/v2.0`.

Do not reuse a Teams bot's app-only Graph credential for user-scoped Microsoft
OAuth — each staff member consents their own Graph access.

## Grant The Credential

Consent does not automatically grant the token to every session. In the
console, open **Principals**, choose the user or channel, and use **Direct
Grants** to select the secret created for the credential — or grant it to a
reusable role.

## Disable Or Remove

Toggle **Enabled** off on the app page to stop new consent flows; existing
credentials keep working. To fully remove access, revoke grants to the wrapper
secret, delete it, then delete the credential.
