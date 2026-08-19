require "test_helper"

module Broker
  class AuthorizationCodeClientTest < ActiveSupport::TestCase
    def client_with(status:, body:)
      http = expect_http_call(status: status, body: body) { |request| yield request if block_given? }
      [ AuthorizationCodeClient.new(http: http), http ]
    end

    def base_args(**overrides)
      {
        token_endpoint: "https://oauth2.googleapis.com/token",
        client_id: "cid", client_secret: "sec",
        code: "auth-code", redirect_uri: "https://control.example/oauth/google/callback",
        code_verifier: "verifier"
      }.merge(overrides)
    end

    def success_body(**overrides)
      {
        access_token: "AT", refresh_token: "RT", expires_in: 3600,
        scope: "openid email", id_token: "the.id.token"
      }.merge(overrides).to_json
    end

    test "happy path parses the authorization_code response" do
      client, http = client_with(status: 200, body: success_body) do |request|
        form = request[:form]
        assert_equal "authorization_code", form["grant_type"]
        assert_equal "auth-code", form["code"]
        assert_equal "cid", form["client_id"]
        assert_equal "sec", form["client_secret"]
        assert_equal "https://control.example/oauth/google/callback", form["redirect_uri"]
        assert_equal "verifier", form["code_verifier"]
      end
      result = client.exchange(**base_args)
      http.verify
      assert_equal "AT", result.access_token
      assert_equal "RT", result.refresh_token
      assert_equal 3600, result.expires_in
      assert_equal "openid email", result.scope
      assert_equal "the.id.token", result.id_token
      assert_equal "AT", result.response["access_token"]
    end

    test "missing expires_in yields nil" do
      client, http = client_with(status: 200, body: success_body(expires_in: nil))
      assert_nil client.exchange(**base_args).expires_in
      http.verify
    end

    test "OAuth error body raises an oauth ExchangeError" do
      client, http = client_with(status: 400, body: { error: "invalid_grant" }.to_json)
      err = assert_raises(ExchangeError) { client.exchange(**base_args) }
      http.verify
      assert_equal "oauth", err.stage
      assert_equal "invalid_grant", err.code
      assert_equal "invalid_grant", err.reason
    end

    test "Slack-style ok false response raises an oauth ExchangeError" do
      client, http = client_with(status: 200, body: { ok: false, error: "invalid_code" }.to_json)
      err = assert_raises(ExchangeError) { client.exchange(**base_args) }
      http.verify
      assert_equal "oauth", err.stage
      assert_equal "invalid_code", err.code
    end

    test "5xx raises an http ExchangeError" do
      client, http = client_with(status: 503, body: "upstream down")
      err = assert_raises(ExchangeError) { client.exchange(**base_args) }
      http.verify
      assert_equal "http", err.stage
      assert_equal 503, err.status
    end

    test "missing refresh_token is a misconfiguration error" do
      client, http = client_with(status: 200, body: success_body(refresh_token: nil))
      err = assert_raises(ExchangeError) { client.exchange(**base_args) }
      http.verify
      assert_equal "missing_refresh_token", err.code
    end

    test "require_refresh_token: false tolerates a missing refresh_token (login flow)" do
      client, http = client_with(status: 200, body: success_body(refresh_token: nil))
      result = client.exchange(**base_args, require_refresh_token: false)
      http.verify
      assert_equal "AT", result.access_token
      assert_nil result.refresh_token
    end

    test "omits code_verifier when the caller does not use PKCE" do
      client, http = client_with(status: 200, body: success_body) do |request|
        assert_nil request[:form]["code_verifier"]
      end
      client.exchange(**base_args(code_verifier: nil))
      http.verify
    end

    test "parses Slack nested authed_user token payload" do
      body = {
        ok: true,
        access_token: "BOT",
        refresh_token: "BOT-RT",
        expires_in: 43_200,
        scope: "commands",
        authed_user: {
          id: "U123",
          access_token: "USER",
          refresh_token: "USER-RT",
          expires_in: 43_200,
          scope: "channels:history,im:history"
        }
      }.to_json

      client, http = client_with(status: 200, body: body)
      result = client.exchange(**base_args)
      http.verify
      assert_equal "USER", result.access_token
      assert_equal "USER-RT", result.refresh_token
      assert_equal "channels:history,im:history", result.scope
      assert_equal "U123", result.response.dig("authed_user", "id")
    end

    test "empty access_token raises a parse error" do
      client, http = client_with(status: 200, body: success_body(access_token: ""))
      err = assert_raises(ExchangeError) { client.exchange(**base_args) }
      http.verify
      assert_equal "parse", err.stage
    end

    test "unparseable body raises a parse error" do
      client, http = client_with(status: 200, body: "not json{")
      err = assert_raises(ExchangeError) { client.exchange(**base_args) }
      http.verify
      assert_equal "parse", err.stage
    end

    test "validates required inputs" do
      client = AuthorizationCodeClient.new(http: Minitest::Mock.new)
      assert_raises(ArgumentError) { client.exchange(**base_args(code: "")) }
      assert_raises(ArgumentError) { client.exchange(**base_args(redirect_uri: "")) }
    end
  end
end
