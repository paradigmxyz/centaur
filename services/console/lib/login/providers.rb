module Login
  # Registry of console-login provider strategies. A strategy owns the
  # IdP-specific parts of the login flow (endpoints, scopes, id_token identity
  # extraction and whether the authorization request uses PKCE); state signing,
  # the code exchange, and user provisioning live in SessionOauthController.
  module Providers
    def self.registry
      @registry ||= { Google::KEY => Google.new, Slack::KEY => Slack.new }.freeze
    end

    # The strategy for +key+, or nil for an unknown provider.
    def self.fetch(key) = registry[key]

    def self.keys = registry.keys
  end
end
