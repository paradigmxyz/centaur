require "test_helper"

module Oauth
  class ProvidersTest < ActiveSupport::TestCase
    # token-broker refreshes flow-minted credentials with its own table of
    # providers; one missing there would never refresh.
    #
    # Runlayer refreshes must carry the RFC 8707 resource and defer or die when
    # RUNLAYER_MCP_URL changes (Providers::Runlayer#refresh_resource), which
    # token-broker does not implement, so Runlayer stays on Console's refresh.
    CONSOLE_REFRESHED = [ Providers::Runlayer::KEY ].freeze

    test "token-broker knows how to refresh every provider's credentials" do
      table = JSON.parse(Rails.root.join("../../crates/centaur-token-broker/providers.json").read)
      registry = Providers.registry.except(*CONSOLE_REFRESHED)

      assert_equal registry.keys.sort, table.keys.sort
      registry.each do |key, provider|
        auth_method = provider.respond_to?(:token_endpoint_auth_method) ? provider.token_endpoint_auth_method : "client_secret_post"
        assert_equal auth_method, table.dig(key, "token_endpoint_auth_method"), key
        assert_equal provider.refresh_scopes(%w[scope]).present?, table.dig(key, "refresh_scopes"), key
      end
    end
  end
end
