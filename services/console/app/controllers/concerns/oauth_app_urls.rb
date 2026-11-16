# The console's public origin and the OAuth consent-flow URLs built on it.
# Shared by ApplicationController (the redirect URIs the SSO, broker OAuth, and
# MCP OAuth endpoints send to providers and clients) and the /api/v1 OAuth app
# endpoints (the redirect URI and start URL shown to operators), so the two
# never drift. The public origin is CENTAUR_CONSOLE_PUBLIC_URL when set (for
# proxies whose Host header doesn't match the public origin), else the
# request's base URL.
module OauthAppUrls
  private

  def public_base_url
    ConsoleEnv["PUBLIC_URL"].presence || request.base_url
  end

  # The redirect URI registered with the provider: "<base>/oauth/<slug>/callback".
  def oauth_callback_redirect_uri(slug)
    URI.join(public_base_url, "/oauth/#{slug}/callback").to_s
  end

  # The well-known consent link: "<base>/oauth/<slug>/start".
  def oauth_start_url(slug)
    URI.join(public_base_url, "/oauth/#{slug}/start").to_s
  end
end
