require "test_helper"

module Oauth
  module Providers
    class HubspotTest < ActiveSupport::TestCase
      def result(access_token: "hub-token", scope: "oauth crm.objects.contacts.read")
        Broker::AuthorizationCodeClient::Result.new(
          access_token: access_token, refresh_token: "hub-refresh", expires_in: 1800,
          scope: scope, id_token: nil, response: {}
        )
      end

      test "builds a deterministic pending identity without calling HubSpot" do
        identity = Hubspot.new.identity_from(result, client_id: "unused")

        assert_match(/\Apending-[a-f0-9]{32}\z/, identity[:subject])
        assert_nil identity[:email]
        assert_equal "Pending HubSpot account", identity[:name]
        assert_equal identity, Hubspot.new.identity_from(result, client_id: "unused")
      end

      test "missing access token raises a parse error" do
        err = assert_raises(Broker::ExchangeError) do
          Hubspot.new.identity_from(result(access_token: nil), client_id: "unused")
        end
        assert_equal "missing_access_token", err.code
      end

      test "parses space separated granted scopes" do
        assert_equal %w[oauth crm.objects.contacts.read],
                     Hubspot.new.parse_granted_scopes("oauth crm.objects.contacts.read")
      end

      test "exposes provider constants" do
        strategy = Hubspot.new
        assert_equal "hubspot", strategy.key
        assert_equal "HubSpot", strategy.display_name
        assert_equal "https://app.hubspot.com/oauth/authorize", strategy.authorization_endpoint
        assert_equal "https://api.hubapi.com/oauth/v3/token", strategy.token_endpoint
        assert_equal [], strategy.identity_scopes
        assert_equal %w[api.hubapi.com api.hubspot.com], strategy.api_hosts
        assert_equal "scope", strategy.authorization_scope_param
        assert_equal " ", strategy.scope_separator
        assert_equal({}, strategy.extra_authorization_params)
        assert strategy.refreshable?
        assert_equal %w[oauth crm.objects.contacts.read],
                     strategy.refresh_scopes(%w[oauth crm.objects.contacts.read])
      end
    end
  end
end
