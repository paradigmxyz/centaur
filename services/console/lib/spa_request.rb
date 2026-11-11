# Route constraint for the console SPA fallback (config/routes/spa.rb): any
# browser navigation (GET/HEAD that accepts HTML) that no Rails route claimed
# serves the SPA, which renders its own not-found page. API, framework, and
# protocol namespaces (MCP OAuth, broker OAuth, SSO) and the server-rendered
# login pages keep the JSON 404 for unknown paths, as on origin/main, so
# machine clients never get HTML.
#
# Legacy Rails routes declared earlier still win for their paths.
module SpaRequest
  EXCLUDED_PREFIXES = %w[
    /api
    /rails
    /spa
    /assets
    /.well-known
    /up
    /mcp
    /oauth
    /auth
    /login
    /logout
    /pending
  ].freeze

  module_function

  def matches?(request)
    return false unless request.get? || request.head?
    return false unless request.format.html?

    spa_path?(request.path)
  end

  def spa_path?(path)
    # "/mcp", "/mcp/..." and format variants such as "/mcp.html" all stay out.
    EXCLUDED_PREFIXES.none? do |prefix|
      path == prefix || path.start_with?("#{prefix}/") || path.start_with?("#{prefix}.")
    end
  end
end
