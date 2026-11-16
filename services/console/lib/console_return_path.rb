require "uri"

# Allowlist for the same-origin paths a browser may be sent to after signing in
# (session[:return_to], the login page's ?return_to=) and from the PWA launch
# handler. Anything else falls back to the caller's default, so a crafted link
# can never bounce an operator off-origin or onto an arbitrary Rails endpoint.
#
# Allowed: "/", the console SPA ("/console", "/console/..."), and the GET
# entry points that require a session and remember where they were going: the
# MCP OAuth authorize endpoint (also as /mcp/oauth/authorize.html, which the
# route accepts too), broker OAuth consent starts, and /launch. The query is
# kept verbatim.
module ConsoleReturnPath
  MAX_LENGTH = 4096

  module_function

  # The OAuth app slug format (OauthApp::URL_SAFE_FORMAT) without its anchors,
  # so /oauth/<slug>/start accepts every slug an app can have (including "~").
  def oauth_slug_source
    OauthApp::URL_SAFE_FORMAT.source.delete_prefix("\\A").delete_suffix("\\z")
  end

  def allowed_paths
    @allowed_paths ||= [
      %r{\A/\z},
      %r{\A/console(?:/[A-Za-z0-9_\-./~%:@]*)?\z},
      %r{\A/mcp/oauth/authorize(?:\.html)?\z},
      %r{\A/oauth/#{oauth_slug_source}/(?:start|callback)(?:\.html)?\z},
      %r{\A/launch\z}
    ].freeze
  end

  # The normalized path (with its query) when +raw+ is an allowed same-origin
  # return path, else nil.
  def safe(raw)
    value = raw.to_s
    return nil if value.empty? || value.length > MAX_LENGTH
    # Fragments never reach the server and are dropped from the return path.
    value = value.split("#", 2).first.to_s
    path, query = value.split("?", 2)
    # Browsers treat "\" like "/" and strip tabs/newlines in the path, so
    # "/\evil.example" or "/\t/evil.example" can turn into a protocol-relative
    # URL. The query is opaque to that resolution (OAuth clients put arbitrary
    # state there, sometimes unencoded), so only control characters and spaces,
    # which could split a Location header, are refused in it.
    return nil if path.to_s.match?(/[\\[:cntrl:][:space:]]/)
    return nil if query&.match?(/[[:cntrl:][:space:]]/)
    return nil unless path.start_with?("/") && !path.start_with?("//")

    uri = URI.parse(path)
    return nil if uri.scheme || uri.host || uri.userinfo || uri.port || uri.query

    return nil if dot_segment?(uri.path.to_s)
    return nil unless allowed_paths.any? { |pattern| pattern.match?(uri.path.to_s) }

    query.nil? ? path : "#{path}?#{query}"
  rescue URI::InvalidURIError
    nil
  end

  # "." / ".." segments (literal or percent-encoded) are resolved by the
  # browser, so "/console/../api" would escape the allowlisted prefix.
  def dot_segment?(path)
    path.match?(/%2e/i) || path.split("/").any? { |segment| segment == "." || segment == ".." }
  end
end
