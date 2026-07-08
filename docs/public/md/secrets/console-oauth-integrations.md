---
title: Console OAuth Integrations
description: How the Centaur Console Integrations page turns provider OAuth consent into grantable, proxy-injected user credentials.
---

# Console OAuth Integrations

The Centaur Console **Integrations** page lets a console user connect an upstream
account or workspace with OAuth. A successful connection mints a managed
`BrokerCredential`, wraps it in a grantable `StaticSecret`, and lets
[iron-proxy](https://docs.iron.sh) inject the current access token into agent
requests to that provider's API hosts. Supported providers: Google, Slack,
GitHub, Granola, Linear, and Attio.

## Set it up in Console

1. Open **Integrations** in the Centaur Console and click **Connect** on the
   provider you want (adjust the requested scopes first if the provider offers
   a scope picker).
2. Approve the consent screen on the provider's site. You are redirected back
   to Console, which stores the credential and wraps it in a grantable secret.
3. Grant the resulting secret to the principals that need it (many providers
   auto-grant to your matching user by email or provider subject). iron-proxy
   then injects a live bearer token into agent requests to that provider's API
   hosts.

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
