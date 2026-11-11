# Route constraint for the console SPA catch-all (config/routes/spa.rb): a
# browser navigation (GET/HEAD that accepts HTML) to a path the SPA owns.
#
# Add a prefix here when a slice moves pages to the SPA under a new top-level
# path. Legacy Rails routes declared earlier still win for their paths.
module SpaRequest
  PATH_PREFIXES = %w[
    /console
    /login
    /pending
    /oauth
    /mcp/oauth
  ].freeze

  module_function

  def matches?(request)
    return false unless request.get? || request.head?
    return false unless request.format.html?

    spa_path?(request.path)
  end

  def spa_path?(path)
    path == "/" || PATH_PREFIXES.any? { |prefix| path == prefix || path.start_with?("#{prefix}/") }
  end
end
