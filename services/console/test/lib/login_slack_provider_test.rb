require "test_helper"

module Login
  module Providers
    class SlackTest < ActiveSupport::TestCase
      CLIENT_ID = "slack-login-client-id".freeze

      def result(claims)
        payload = Base64.urlsafe_encode64(claims.to_json, padding: false)
        Broker::AuthorizationCodeClient::Result.new(
          access_token: "AT", refresh_token: nil, expires_in: 3600,
          scope: "openid email profile", id_token: "h.#{payload}.s", response: {}
        )
      end

      test "extracts the workspace id from Slack's verified OIDC identity" do
        identity = Slack.new.identity_from(
          result(
            "aud" => CLIENT_ID,
            "iss" => "https://slack.com",
            "sub" => "U123",
            "email" => "ada@tempo.xyz",
            "email_verified" => true,
            "https://slack.com/team_id" => "T123"
          ),
          client_id: CLIENT_ID
        )

        assert_equal "U123", identity[:subject]
        assert_equal "T123", identity[:team_id]
      end
    end
  end
end
