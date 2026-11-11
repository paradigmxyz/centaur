# Auth pages slice: console login/logout, pending holding page, SSO,
# broker OAuth consent flow, MCP OAuth authorization server, and PWA/launch.
# Protocol endpoints (MCP token/register/metadata, OAuth callbacks) stay as
# Rails routes; their HTML result pages move to the SPA.

# PWA manifest + service worker, rendered from app/views/pwa/*. Served by
# Rails::PwaController (framework controller, no console session required) so
# the browser can fetch them outside an authenticated page load. Both layouts
# link the manifest and register the worker.
get "manifest" => "rails/pwa#manifest", as: :pwa_manifest
get "service-worker" => "rails/pwa#service_worker", as: :pwa_service_worker
# Target of the manifest's web+centaur:// protocol handler: maps the
# custom-scheme URL in ?target= onto an in-app path and redirects.
get "launch", to: "launch#show", as: :launch

# Operator console session login (cookie-based, separate from the API key auth).
get "login", to: "sessions#new", as: :login
post "login", to: "sessions#create"
delete "logout", to: "sessions#destroy", as: :logout
# Holding page for a signed-in user whose account is still pending approval.
get "pending", to: "sessions#pending", as: :pending

# Console SSO login, keyed by provider (/auth/google/start). Deliberately
# unauthenticated; distinct /auth/* prefix avoids colliding with the broker's
# /oauth/:slug/* consent flow below.
get "auth/:provider/start", to: "session_oauth#start", as: :auth_start
get "auth/:provider/callback", to: "session_oauth#callback", as: :auth_callback

# MCP OAuth authorization server. MCP clients discover this from api-rs'
# OAuth protected-resource metadata and register public PKCE clients here.
get ".well-known/oauth-authorization-server", to: "mcp/oauth#metadata"
get ".well-known/openid-configuration", to: "mcp/oauth#metadata"
post "mcp/oauth/register", to: "mcp/oauth#register"
get "mcp/oauth/authorize", to: "mcp/oauth#authorize"
post "mcp/oauth/authorize", to: "mcp/oauth#approve"
post "mcp/oauth/token", to: "mcp/oauth#token"

# OAuth consent flow, keyed by the app's well-known slug (/oauth/google/start).
# Requires an active console session; the provider is derived from the app.
get "oauth/:slug/start", to: "oauth/flows#start", as: :oauth_start
get "oauth/:slug/callback", to: "oauth/flows#callback", as: :oauth_callback
