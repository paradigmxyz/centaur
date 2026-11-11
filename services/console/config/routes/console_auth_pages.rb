# Auth pages slice: console login/logout, the pending holding page, SSO, the
# broker OAuth consent flow, the MCP OAuth authorization server, and PWA/launch.
#
# Sign-in is part of the MCP OAuth flow, so the login and pending pages are
# intentionally server-rendered (layouts/oauth), like the MCP OAuth consent page
# and the broker OAuth result page. They keep origin/main's wire contract
# (Rails form + authenticity token, the same paths, params, redirects, and
# messages), work without JavaScript, and interoperate with older pods during a
# rolling deploy. Everything after sign-in is the SPA.

# Operator console session login (cookie-based, separate from the API key auth).
get "login", to: "sessions#new", as: :login
post "login", to: "sessions#create"
delete "logout", to: "sessions#destroy", as: :logout
# Holding page for a signed-in user whose account is still pending approval.
get "pending", to: "sessions#pending", as: :pending

# PWA manifest + service worker: static files under config/pwa served without a
# console session (PwaController) so the browser can fetch them outside an
# authenticated page load. The SPA links the manifest and registers the worker.
get "manifest", to: "pwa#manifest", as: :pwa_manifest
get "service-worker", to: "pwa#service_worker", as: :pwa_service_worker
# Target of the manifest's web+centaur:// protocol handler: maps the
# custom-scheme URL in ?target= onto an in-app path and redirects.
get "launch", to: "launch#show", as: :launch

# The SPA's sign-out button uses DELETE /api/v1/session (api.rb).

# Console SSO login, keyed by provider (/auth/google/start). Deliberately
# unauthenticated; distinct /auth/* prefix avoids colliding with the broker's
# /oauth/:slug/* consent flow below. Failures land on /login with a flash.
get "auth/:provider/start", to: "session_oauth#start", as: :auth_start
get "auth/:provider/callback", to: "session_oauth#callback", as: :auth_callback

# MCP OAuth authorization server. MCP clients discover this from api-rs'
# OAuth protected-resource metadata and register public PKCE clients here.
# GET authorize validates the request and renders the consent page; POST
# authorize (the consent form) issues the code and redirects to the registered
# redirect_uri.
get ".well-known/oauth-authorization-server", to: "mcp/oauth#metadata"
get ".well-known/openid-configuration", to: "mcp/oauth#metadata"
post "mcp/oauth/register", to: "mcp/oauth#register"
get "mcp/oauth/authorize", to: "mcp/oauth#authorize"
post "mcp/oauth/authorize", to: "mcp/oauth#approve"
post "mcp/oauth/token", to: "mcp/oauth#token"

# OAuth consent flow, keyed by the app's well-known slug (/oauth/google/start).
# Requires an active console session; the provider is derived from the app.
# Failures render a server-side result page.
# Slugs may contain any OauthApp::URL_SAFE_FORMAT character, including "." and
# "~"; the controller resolves (or 404s) the slug.
get "oauth/:slug/start", to: "oauth/flows#start", as: :oauth_start, constraints: { slug: %r{[^/]+} }
get "oauth/:slug/callback", to: "oauth/flows#callback", as: :oauth_callback, constraints: { slug: %r{[^/]+} }
