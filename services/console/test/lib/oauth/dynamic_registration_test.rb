require "test_helper"

class Oauth::DynamicRegistrationTest < ActiveSupport::TestCase
  def register(http)
    Oauth::DynamicRegistration.new(http: HttpClient.new(http: http)).register(
      endpoint: "https://provider.example/register", redirect_uri: "https://console.example/oauth/example/callback",
      scope: "read", auth_method: "client_secret_post")
  end

  test "registers the exact callback and validates confidential client credentials" do
    http = expect_http_call(status: 201, body: { client_id: "id", client_secret: "synthetic" }.to_json) do |request|
      body = JSON.parse(request[:body])
      assert_equal [ "https://console.example/oauth/example/callback" ], body["redirect_uris"]
      assert_equal "client_secret_post", body["token_endpoint_auth_method"]
      assert_equal "read", body["scope"]
    end
    assert_equal %i[client_id client_secret], register(http).keys
    http.verify
  end

  test "fails safely on HTTP JSON credentials and network errors" do
    [ [ 500, "{}" ], [ 200, "bad json" ], [ 200, "[]" ],
      [ 200, '{"client_id":"id","client_secret":" "}' ], [ 200, '{"client_id":42,"client_secret":"secret"}' ] ].each do |status, body|
      http = expect_http_call(status: status, body: body)
      assert_raises(Broker::ExchangeError) { register(http) }
      http.verify
    end
    assert_raises(Broker::ExchangeError) { register(->(**) { raise Timeout::Error }) }
  end
end
