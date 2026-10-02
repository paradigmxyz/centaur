require "test_helper"

class Mercator::ConnectionClientTest < ActiveSupport::TestCase
  test "registers a confidential client with only the configured callback and tool scope" do
    client = Mercator::ConnectionClient.new(http: HttpClient.new(http: ->(**request) {
      assert_equal "https://mercator.sh/oauth/register", request[:url]
      body = JSON.parse(request[:body])
      assert_equal [ "https://console.example/oauth/mercator/callback" ], body["redirect_uris"]
      assert_equal "client_secret_post", body["token_endpoint_auth_method"]
      assert_equal "mercator:tools", body["scope"]
      HttpClient::Response.new(status: 201, body: { client_id: "id", client_secret: "synthetic" }.to_json)
    }))
    assert_equal %w[client_id client_secret], client.register("https://console.example/oauth/mercator/callback").keys
  end

  test "status accepts JSON and SSE without exposing payment credentials" do
    [ "application/json", "text/event-stream" ].each do |content_type|
      payload = { jsonrpc: "2.0", result: { structuredContent: { oauthAuthenticated: true } } }.to_json
      body = content_type == "text/event-stream" ? "event: message\ndata: #{payload}\n\n" : payload
      client = Mercator::ConnectionClient.new(http: HttpClient.new(http: ->(**request) {
        assert_equal "https://mercator.sh/mcp/auth", request[:url]
        assert_equal "get_connection_status", JSON.parse(request[:body]).dig("params", "name")
        HttpClient::Response.new(status: 200, body: body, headers: { "content-type" => content_type })
      }))
      assert_equal true, client.status("synthetic")["oauthAuthenticated"]
    end
  end

  test "malformed, unauthorized and protocol error responses fail closed" do
    [ [ 401, "unauthorized" ], [ 200, "not json" ], [ 200, '{"result":{"isError":true}}' ] ].each do |status, body|
      client = Mercator::ConnectionClient.new(http: HttpClient.new(http: ->(**) {
        HttpClient::Response.new(status: status, body: body)
      }))
      assert_raises(Broker::ExchangeError) { client.status("synthetic") }
    end
  end
  test "Slack status only disables claims for the matching wallet reservation" do
    wallet = "0x#{'12' * 20}"
    [ [ wallet, true, true ], [ wallet.upcase, true, true ], [ wallet, false, false ], [ "other", true, false ] ].each do |address, claimed, expected|
      client = Mercator::ConnectionClient.new(http: HttpClient.new(http: ->(**request) {
        assert_equal "https://mercator.sh/v1/claims/config?provider=slack", request[:url]
        assert_equal "Bearer synthetic", request[:headers]["Authorization"]
        HttpClient::Response.new(status: 200, body: { wallet_address: address, claimed: claimed }.to_json)
      }))
      assert_equal expected, client.slack_claimed?("synthetic", wallet: wallet)
    end
  end

  test "unavailable Slack status keeps the claim link available" do
    [ [ 401, "unauthorized" ], [ 503, "unavailable" ], [ 200, "not json" ] ].each do |status, body|
      client = Mercator::ConnectionClient.new(http: HttpClient.new(http: ->(**) {
        HttpClient::Response.new(status: status, body: body)
      }))
      assert_not client.slack_claimed?("synthetic", wallet: "0x#{'12' * 20}")
    end
  end
end
