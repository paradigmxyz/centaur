# Auth pages slice: console login/logout, the pending holding page, SSO, the
# broker OAuth consent flow, the MCP OAuth authorization server, and PWA/launch.
#
# The pages themselves (/login, /pending, /oauth/result, /mcp/oauth/consent)
# live in the SPA (web/src/features/auth-pages) and are served by the SPA
# fallback. Rails keeps the protocol endpoints (redirects and JSON) and the
# path helpers legacy redirects use.
direct(:login) { "/login" }
direct(:pending) { "/pending" }

# PWA manifest + service worker: static files under config/pwa served without a
# console session (PwaController) so the browser can fetch them outside an
# authenticated page load. The SPA links the manifest and registers the worker.
get "manifest", to: "pwa#manifest", as: :pwa_manifest
get "service-worker", to: "pwa#service_worker", as: :pwa_service_worker
# Target of the manifest's web+centaur:// protocol handler: maps the
# custom-scheme URL in ?target= onto an in-app path and redirects.
get "launch", to: "launch#show", as: :launch

# Redirect-only form endpoints: password sign-in (the SPA uses
# POST /api/v1/session) and sign-out for the legacy account menu (the SPA uses
# DELETE /api/v1/session).
post "login", to: "sessions#create"
delete "logout", to: "sessions#destroy", as: :logout

# Console SSO login, keyed by provider (/auth/google/start). Deliberately
# unauthenticated; distinct /auth/* prefix avoids colliding with the broker's
# /oauth/:slug/* consent flow below. Failures land on /login with a flash.
get "auth/:provider/start", to: "session_oauth#start", as: :auth_start
get "auth/:provider/callback", to: "session_oauth#callback", as: :auth_callback

# MCP OAuth authorization server. MCP clients discover this from api-rs'
# OAuth protected-resource metadata and register public PKCE clients here.
# GET authorize validates the request and hands a signed consent ticket to the
# SPA consent page (/mcp/oauth/consent), which posts the decision as a form to
# POST /mcp/oauth/consent; that answers with a 303 to the registered
# redirect_uri.
get ".well-known/oauth-authorization-server", to: "mcp/oauth#metadata"
get ".well-known/openid-configuration", to: "mcp/oauth#metadata"
post "mcp/oauth/register", to: "mcp/oauth#register"
get "mcp/oauth/authorize", to: "mcp/oauth#authorize"
post "mcp/oauth/consent", to: "mcp/oauth#decide"
post "mcp/oauth/token", to: "mcp/oauth#token"

# OAuth consent flow, keyed by the app's well-known slug (/oauth/google/start).
# Requires an active console session; the provider is derived from the app.
# Failures land on the SPA result page (/oauth/result) with a flash.
# Slugs may contain any OauthApp::URL_SAFE_FORMAT character, including "." and
# "~"; the controller resolves (or 404s) the slug.
get "oauth/:slug/start", to: "oauth/flows#start", as: :oauth_start, constraints: { slug: %r{[^/]+} }
get "oauth/:slug/callback", to: "oauth/flows#callback", as: :oauth_callback, constraints: { slug: %r{[^/]+} }

# /api/v1 endpoints for the auth pages (see docs/API.md#console-auth-pages).
namespace :api do
  namespace :v1 do
    get "auth/config", to: "auth#show", as: :auth_config
    resource :session, only: :create
    get "mcp/authorization", to: "mcp_authorizations#show", as: :mcp_authorization
  end
end
