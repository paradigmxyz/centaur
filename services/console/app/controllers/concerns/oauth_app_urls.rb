# Public consent-flow URLs for OAuth apps, for /api/v1 controllers (the HTML
# side has the same helpers on ApplicationController). The public origin is
# CENTAUR_CONSOLE_PUBLIC_URL when set, else the request's base URL.
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
