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

## Grant The Credential

Consent does not automatically grant the token to every session. In the
console, open **Principals**, choose the user or channel, and use **Direct
Grants** to select the secret created for the credential — or grant it to a
reusable role.

## Disable Or Remove

Toggle **Enabled** off on the app page to stop new consent flows; existing
credentials keep working. To fully remove access, revoke grants to the wrapper
secret, delete it, then delete the credential.
