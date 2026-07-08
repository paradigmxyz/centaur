---
title: Console OAuth Integrations
description: How the Centaur Console Integrations page turns provider OAuth consent into grantable, proxy-injected user credentials.
---

# Console OAuth Integrations

The Centaur Console **Integrations** page lets a console user connect an upstream
account or workspace with OAuth. A successful connection mints a managed
`BrokerCredential`, wraps it in a grantable `StaticSecret`, and lets
[iron-proxy](https://docs.iron.sh) inject the current access token into agent
requests to that provider's API hosts.

This page documents the provider strategies in
`services/console/lib/oauth/providers/*.rb`, the consent controller in
`services/console/app/controllers/oauth/flows_controller.rb`, and the provider
rows seeded from `services/console/db/seeds.rb`. Google, Slack, GitHub, and
Granola are on `main`; Linear and Attio follow the same pattern in their provider
PRs.

## End-to-end flow

1. **Provider app registration.** Operators create or seed an `OauthApp` with a
   provider key, client id, encrypted client secret, allowed scopes, credential
   namespace, and enabled flag. The console uses the app slug for
   `/oauth/<slug>/start` and `/oauth/<slug>/callback`.
2. **Consent redirect.** `Oauth::FlowsController#start` validates the requested
   scopes against `OauthApp#allowed_scopes`, signs short-lived state, stores a
   PKCE verifier in an encrypted cookie, and redirects to the provider strategy's
   authorization endpoint.
3. **Code exchange.** `#callback` verifies state and the PKCE cookie, exchanges
   the authorization code through `Broker::AuthorizationCodeClient`, and asks the
   provider strategy for a stable identity. Refreshable providers require a
   refresh token from this exchange.
4. **Broker credential upsert.** The controller upserts one `BrokerCredential`
   per `(OauthApp, provider_subject)`, stores granted scopes, access token,
   refresh token, expiry, and refresh state, and schedules `next_attempt_at` only
   when the provider strategy is refreshable.
5. **Wrapper static secret.** `ensure_wrapping_secret` creates one grantable
   `StaticSecret` for the broker credential. Its source is `source_type:
   "token_broker"` with the credential oid, its inject config is
   `Authorization: Bearer {{ .Value }}`, and its request rules are the provider
   strategy's API hosts.
6. **Proxy injection.** Broker credentials are not synced or grantable directly.
   During secret sync, the `token_broker` source resolves to the credential's
   current access token, so iron-proxy injects a live bearer token only for
   matching hosts.
7. **Identity enrichment and grants.** Providers whose token response lacks final
   identity enqueue a job under `services/console/app/jobs/oauth/` to backfill
   the subject, email, name, and wrapper-secret name. `BrokerCredential` then runs
   `PrincipalCredentialReconciliation`, which auto-grants the wrapper secret to
   matching `user` and `console_user` principals by provider subject labels where
   supported, or by email where available.
8. **Refresh loop.** `Broker::PollRefreshJob` finds refreshable credentials and
   drives `Broker::RefreshCredentialJob` / `BrokerCredential#refresh!` before
   expiry. Providers without refresh tokens are live until the upstream token is
   revoked or replaced by re-consent.

## Providers

| Provider | Flow and identity | Refresh | API host rules | Default seeded scopes |
|----------|-------------------|---------|----------------|-----------------------|
| Google | OAuth/OIDC-style. Adds `openid` and `userinfo.email`, validates `id_token` issuer/audience, and uses `sub` plus email from the token. | Yes. Adds `access_type=offline` and `prompt=consent`; refresh reuses granted scopes. | `*.googleapis.com` | Gmail readonly, Calendar readonly, Drive readonly. |
| Slack | Slack OAuth v2 user-token flow using `user_scope`. Uses `authed_user` from `oauth.v2.access` when present, with an enrichment job that can call `auth.test` and `users.info`. | Yes, assuming the Slack app has token rotation enabled. Refresh omits scopes. | `slack.com` | `chat:write`, `channels:history`, `channels:read`, `users:read`. |
| GitHub | GitHub OAuth app flow. The token response has no identity, so the callback stores a pending subject and `EnrichGithubCredentialIdentityJob` calls `https://api.github.com/user`. | No in the current `main` strategy (`refreshable? = false`). Re-consent replaces the access token. | `api.github.com`, `github.com` | `repo`, `read:user`. |
| Granola | OIDC-style OAuth for Granola's MCP auth server. Client credentials come from Dynamic Client Registration at `https://mcp-auth.granola.ai/oauth2/register`; identity comes from the `id_token`. | Yes. Adds `openid`, `email`, `profile`, and `offline_access`; refresh reuses granted scopes. | `mcp.granola.ai` | `mcp`. |
| Linear | OAuth2. The token response has no identity; the callback stores a pending subject and `EnrichLinearCredentialIdentityJob` backfills from GraphQL `{ viewer { id name email } }`. | Yes. Refresh reuses granted scopes. | `api.linear.app` | `read`, `write`. |
| Attio | OAuth2 workspace authorization. The seeded allowlist mirrors scopes configured in the Attio developer dashboard; the generic consent flow also includes requested scopes on the authorize redirect. The token is workspace-scoped and has no user identity, scope echo, refresh token, or expiry, so `EnrichAttioCredentialIdentityJob` calls `/v2/self` and stores the workspace id/name with no provider email. | No. Long-lived access token; re-consent replaces it. | `api.attio.com` | `user_management:read`, `record_permission:read-write`, `object_configuration:read-write`, `list_entry:read-write`, `list_configuration:read-write`, `comment:read-write`, `note:read-write`, `task:read-write`, `meeting:read-write`, `call_recording:read-write`, `webhook:read-write`, `file:read-write`. |

## Provider notes

### Google

`Oauth::Providers::Google` owns Google's auth and token endpoints, requires
offline access, and decodes the `id_token` returned from the token endpoint. It
accepts Google's documented issuer values and requires `aud == client_id` before
using the `sub` claim as the provider subject.

### Slack

`Oauth::Providers::Slack` uses Slack's `user_scope` parameter for normal Slack
API scopes. Do not mix Sign in with Slack scopes such as `openid`, `email`, or
`profile` into these app scopes; Slack rejects requests that mix those with
normal API scopes. The Slack enrichment job only reads email when the granted
scopes include `users:read.email`.

### GitHub

`Oauth::Providers::Github` currently uses the standard GitHub OAuth authorize and
access-token endpoints. Because the token response does not include identity, it
uses a deterministic pending subject derived from the access token and lets
`EnrichGithubCredentialIdentityJob` replace it with the authenticated GitHub user
id, login/name, and email.

### Granola

`Oauth::Providers::Granola` targets Granola's MCP auth server and protected MCP
resource. Operators obtain the OAuth client through Dynamic Client Registration,
then seed or create the `OauthApp` with that client id and secret. The strategy
requests `offline_access` because the console requires a refresh token for this
provider.

### Linear

The Linear provider follows the same pending-identity pattern as GitHub. Its
enrichment job posts a bearer-authenticated GraphQL viewer query to
`https://api.linear.app/graphql`, updates the credential subject/email/name, and
renames the wrapper secret if the operator has not renamed it.

### Attio

Attio credentials are workspace-level, not per-user. The enrichment job calls
`https://api.attio.com/v2/self`, stores `workspace_id` as the provider subject,
uses `workspace_name` or `workspace_slug` as the display name, and deliberately
leaves `provider_email` blank. Because there is no refresh token or expiry in the
Attio token response, the controller does not schedule refresh attempts.

## Adding a provider

1. **Add a strategy class** in `services/console/lib/oauth/providers/<key>.rb`.
   Implement the provider key, display name, authorization/token endpoints,
   identity scopes, API hosts, authorization scope parameter, scope separator,
   `refreshable?`, `parse_granted_scopes`, `refresh_scopes`, and
   `identity_from`.
2. **Register the strategy** in `services/console/lib/oauth/providers.rb` so
   `OauthApp#provider_strategy`, validations, and the console provider select can
   find it.
3. **Seed or create an `OauthApp`** in `services/console/db/seeds.rb` for dev and
   test ergonomics. Production apps should be configured with real provider
   client credentials and the callback URL
   `<CENTAUR_CONSOLE_PUBLIC_URL>/oauth/<slug>/callback`.
4. **Decide how identity is obtained.** If the token response or `id_token`
   contains a stable subject, parse it in `identity_from`. If not, return a
   deterministic pending subject and add an enrichment job under
   `services/console/app/jobs/oauth/` that calls the provider's identity endpoint
   without blocking the callback.
5. **Enqueue enrichment** from `Oauth::FlowsController#enqueue_identity_enrichment`
   when the provider needs a post-callback lookup.
6. **Set API hosts narrowly.** These become request rules on the wrapper
   `StaticSecret`; use only hosts that should receive the bearer token.
7. **Add tests** for the provider strategy, flow callback/start behavior, any
   enrichment job, seeded app data, and the wrapper secret's host rules and
   refresh behavior.
