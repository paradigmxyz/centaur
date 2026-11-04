require "test_helper"

module Oauth
  class ProvidersTest < ActiveSupport::TestCase
    # token-broker refreshes flow-minted credentials with its own table of
    # providers; one missing there would never refresh.
    test "token-broker knows how to refresh every provider's credentials" do
      table = JSON.parse(Rails.root.join("../../crates/centaur-token-broker/providers.json").read)

      assert_equal Providers.keys.sort, table.keys.sort
      Providers.registry.each do |key, provider|
        auth_method = provider.respond_to?(:token_endpoint_auth_method) ? provider.token_endpoint_auth_method : "client_secret_post"
        assert_equal auth_method, table.dig(key, "token_endpoint_auth_method"), key
        assert_equal provider.refresh_scopes(%w[scope]).present?, table.dig(key, "refresh_scopes"), key
      end
    end
  end
end
