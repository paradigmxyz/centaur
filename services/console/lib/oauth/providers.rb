module Oauth
  # Registry of supported OAuth consent-flow providers. A provider strategy owns
  # the IdP-specific parts of the flow: endpoints, extra authorization params,
  # and how to extract a stable identity from the token response. Everything
  # else (state signing, PKCE, the code exchange itself, credential upsert) is
  # provider-agnostic, so adding a provider is a new strategy class plus a
  # registry entry -- not a migration.
  module Providers
    # Memoized so a strategy is built once per process. The strategies are
    # stateless, so sharing one instance across flows is safe.
    def self.registry
      @registry ||= { Google::KEY => Google.new }.freeze
    end

    # The strategy for +key+, or nil for an unknown provider (the flow
    # controller turns that into a 404).
    def self.fetch(key) = registry[key]

    # The supported provider keys, used for the model's inclusion validation and
    # the console provider select.
    def self.keys = registry.keys
  end
end
