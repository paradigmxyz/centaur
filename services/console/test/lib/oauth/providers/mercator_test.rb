require "test_helper"

module Oauth
  module Providers
    class MercatorTest < ActiveSupport::TestCase
      WALLET = "0x#{'ab' * 20}".freeze

      def result(scope: "mercator:tools")
        Broker::AuthorizationCodeClient::Result.new(access_token: "synthetic", scope: scope, refresh_token: "synthetic-refresh", expires_in: 3600, id_token: nil, response: {})
      end

      test "resolves wallet identity from JSON and SSE" do
        [ "application/json", "text/event-stream" ].each do |type|
          body = { result: { structuredContent: { oauthAuthenticated: true, account: { walletAddress: WALLET } } } }.to_json
          body = "event: message\r\ndata: #{body}\r\n\r\n" if type == "text/event-stream"
          http = expect_http_call(status: 200, body: body, headers: { "content-type" => type }) do |request|
            assert_equal "https://mercator.sh/mcp/auth", request[:url]
            assert_equal "Bearer synthetic", request[:headers]["Authorization"]
            assert_equal "get_connection_status", JSON.parse(request[:body]).dig("params", "name")
          end
          identity = Mercator.new.identity_from(result, client_id: "unused", http_client: HttpClient.new(http: http))
          assert_equal WALLET, identity[:subject]
          http.verify
        end
      end

      test "rejects missing invalid and unauthenticated wallet identities" do
        [ {}, { oauthAuthenticated: true, account: { walletAddress: "bad" } },
          { oauthAuthenticated: false, account: { walletAddress: WALLET } }, { oauthAuthenticated: true, account: [] } ].each do |status|
          http = expect_http_call(status: 200, body: { result: { structuredContent: status } }.to_json)
          assert_raises(Broker::ExchangeError) do
            Mercator.new.identity_from(result, client_id: "unused", http_client: HttpClient.new(http: http))
          end
          http.verify
        end
      end

      test "rejects missing scope" do
        [ nil, "", "other" ].each do |scope|
          assert_raises(Broker::ExchangeError) { Mercator.new.validate_result!(result(scope: scope)) }
        end
        Mercator.new.validate_result!(result)
      end

      test "details distinguish claimed unclaimed and unavailable" do
        credential = Struct.new(:access_token, :provider_subject).new("synthetic", WALLET)
        [ [ 200, { claimed: true, wallet_address: WALLET.upcase }, true ],
          [ 200, { claimed: false, wallet_address: WALLET }, false ],
          [ 200, { claimed: true, wallet_address: "other" }, nil ],
          [ 200, { claimed: "true", wallet_address: WALLET }, nil ],
          [ 200, [], nil ], [ 401, {}, nil ] ].each do |status, body, expected|
          http = expect_http_call(status: status, body: body.to_json) do |request|
            assert_equal "https://mercator.sh/v1/claims/config?provider=slack", request[:url]
            assert_equal "Bearer synthetic", request[:headers]["Authorization"]
          end
          details = Mercator.new.details_for(credential, http_client: HttpClient.new(http: http))
          expected.nil? ? assert_nil(details[:claimed]) : assert_equal(expected, details[:claimed])
          http.verify
        end
        http = HttpClient.new(http: ->(**) { raise Timeout::Error })
        assert_nil Mercator.new.details_for(credential, http_client: http)[:claimed]
      end
    end
  end
end
