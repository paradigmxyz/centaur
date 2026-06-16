---
title: OAuth Apps
description: Register OAuth clients, collect user consent, and grant refreshed access tokens to Centaur principals.
---

# OAuth Apps

OAuth apps let users connect their own upstream accounts to Centaur. An operator
registers an OAuth client in the console, shares a consent link, and each user
who completes the flow creates or updates a managed broker credential.

The broker credential owns refresh-token lifecycle. It refreshes access tokens
inside the Centaur Console and exposes only the current access token to iron-proxy
through a `token_broker` secret source. The user's refresh token never leaves
the Centaur Console.

OAuth apps are separate from console login. Console SSO uses
`/auth/<provider>/start` and signs operators into the console. OAuth apps use
`/oauth/<slug>/start` and mint credentials for tools.

## Supported Providers

| Provider | Use |
|----------|-----|
| `google` | Google API credentials, such as Gmail or Drive scopes. |
| `slack` | Slack user-token credentials with normal Slack API scopes. |

Google flows request offline access and force consent so the token response
includes a refresh token. Slack OAuth apps should enable token rotation so the
callback also receives a refresh token.

## Create The Provider App

Create an OAuth client in the upstream provider first.

Register this callback URL:

```text
<CENTAUR_CONSOLE_PUBLIC_URL>/oauth/<slug>/callback
```

For example:

```text
https://control.example.com/oauth/google-drive/callback
```

The slug is the stable name users see in the consent URL. It must contain only
URL-safe characters.

For Slack, use normal Slack API scopes such as `channels:history` or
`users:read`. Do not use Sign in with Slack scopes such as `openid`, `email`, or
`profile` for OAuth apps.

## Register The App In Centaur

In the console, open **OAuth Apps**, then create an app with:

| Field | Meaning |
|-------|---------|
| `Slug` | Globally unique consent-link name, for example `google-drive`. |
| `Provider` | `google` or `slack`. |
| `Client ID` | OAuth client id from the provider. |
| `Client Secret` | OAuth client secret from the provider. Stored encrypted. |
| `Credential Namespace` | Namespace for broker credentials minted by this app. |
| `Allowed Scopes` | One scope per line. Consent requests must be a subset. |
| `Enabled` | Disabled apps reject new consent flows. Existing credentials keep refreshing. |

You can also create the app through the API:

```bash
curl -sS -X POST "$IRON_CONTROL_URL/api/v1/oauth_apps" \
  -H "Authorization: Bearer $IRON_CONTROL_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "data": {
      "slug": "google-drive",
      "description": "Google Drive user access",
      "provider": "google",
      "client_id": "client-id.apps.googleusercontent.com",
      "client_secret": "client-secret",
      "credential_namespace": "default",
      "allowed_scopes": [
        "https://www.googleapis.com/auth/drive.metadata.readonly"
      ],
      "enabled": true,
      "labels": { "team": "platform" }
    }
  }'
```

`client_secret` is write-only. API responses never include it. Updating an app
without a new `client_secret` keeps the stored value.

## Collect User Consent

Share the app start URL with the user:

```text
<CENTAUR_CONSOLE_PUBLIC_URL>/oauth/<slug>/start
```

Omitting `scopes` requests every allowed scope:

```text
https://control.example.com/oauth/google-drive/start
```

To request a subset, pass scopes as a space-separated or comma-separated query
parameter:

```text
https://control.example.com/oauth/google-drive/start?scopes=https://www.googleapis.com/auth/drive.metadata.readonly
```

The start endpoint rejects unknown slugs, disabled apps, and scopes outside the
app allowlist. After provider consent, the callback exchanges the code, records
the provider account identity, and renders a console result page.

Re-consenting with the same app and provider account updates the existing broker
credential instead of creating another one.

## What Gets Created

A successful consent creates or updates:

| Resource | Purpose |
|----------|---------|
| Broker credential | Stores provider identity, scopes, current access token, refresh token, expiry, and refresh state. |
| Static secret | Grantable wrapper that injects `Authorization: Bearer <access token>`. |

The static secret uses a `token_broker` source that points at the broker
credential. At proxy sync time, the Centaur Console resolves the broker credential and
sends the current access token to iron-proxy. If the credential is still
bootstrapping or cannot refresh, the secret is omitted from proxy config until
it recovers.

The auto-created request rules are provider-scoped:

| Provider | Default API host rules |
|----------|------------------------|
| Google | `*.googleapis.com` |
| Slack | `slack.com` |

Operators can tighten the static secret's rules in the console if a credential
should only be valid for specific API paths.

## Grant The OAuth Credential

OAuth consent does not automatically grant the token to every session. Grant the
auto-created static secret to the correct user, channel, or role.

You can grant the secret in the Centaur Console. Open **Principals**, choose the
user or channel principal, then use **Direct Grants** to select the static secret
created for the broker credential. The same principal page can assign a role if
you grant the OAuth secret to a reusable role instead.

For scripted changes, list secrets in the credential namespace and find the
static secret created for the broker credential:

```bash
curl -sS "$IRON_CONTROL_URL/api/v1/static_secrets?namespace=default" \
  -H "Authorization: Bearer $IRON_CONTROL_API_KEY" | jq
```

Then grant the secret with `centaur-perms`:

```bash
cd services/api-rs
cargo run -p centaur-perms -- \
  principals grant slack-user-u123 \
  --secret ssr_...
```

Grant the same credential to a channel when the channel should define access:

```bash
cargo run -p centaur-perms -- \
  principals grant slack-channel-c456 \
  --secret ssr_...
```

Or grant it to a reusable role:

```bash
cargo run -p centaur-perms -- \
  roles grant tool-google-drive \
  --secret ssr_...
```

## Rotate Or Disable

Rotating the OAuth client's secret on the app updates every credential minted by
that app because minted broker credentials delegate `client_id` and
`client_secret` back to the app.

Disable an app to stop new consent flows:

```bash
curl -sS -X PATCH "$IRON_CONTROL_URL/api/v1/oauth_apps/google-drive" \
  -H "Authorization: Bearer $IRON_CONTROL_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{ "data": { "enabled": false } }'
```

Existing broker credentials keep refreshing while the app exists. To fully
remove access, revoke grants to the wrapper static secret, delete the wrapper
secret, then delete or unlink the broker credential. An app cannot be deleted
while minted credentials still reference it.
