require "test_helper"

module Broker
  class RefreshClientTest < ActiveSupport::TestCase
    # A stub HTTP backend matching RefreshClient's injected contract. Captures the
    # request so tests can assert the form/headers without a real socket.
    class StubHTTP
      attr_reader :captured

      def initialize(status:, body:)
        @status = status
        @body = body
      end

      def call(url:, form:, headers:, timeout:, form_encoding:)
        @captured = { url: url, form: form, headers: headers, timeout: timeout, form_encoding: form_encoding }
        Broker::RefreshClient::Response.new(status: @status, body: @body)
      end
    end

    def client_with(status:, body:)
      http = StubHTTP.new(status: status, body: body)
      [ Broker::RefreshClient.new(http: http), http ]
    end

    def base_args(**overrides)
      {
        token_endpoint: "https://idp.example/token",
        client_id: "cid",
        refresh_token: "rt-old"
      }.merge(overrides)
    end

    test "successful refresh parses the RFC 6749 body" do
      client, _ = client_with(status: 200, body: { access_token: "AT", refresh_token: "RT", expires_in: 3600 }.to_json)
      result = client.refresh(**base_args)
      assert_equal "AT", result.access_token
      assert_equal "RT", result.refresh_token
      assert_equal 3600, result.expires_in
    end

    test "form carries the refresh_token grant and optional fields" do
      client, http = client_with(status: 200, body: { access_token: "AT", expires_in: 60 }.to_json)
      client.refresh(**base_args(client_secret: "sec", scopes: %w[a b], headers: { "X-Api-Key" => "k" }))
      form = http.captured[:form]
      assert_equal "refresh_token", form["grant_type"]
      assert_equal "rt-old", form["refresh_token"]
      assert_equal "cid", form["client_id"]
      assert_equal "sec", form["client_secret"]
      assert_equal "a b", form["scope"]
      assert_equal "k", http.captured[:headers]["X-Api-Key"]
      assert_equal :urlencoded, http.captured[:form_encoding]
    end

    test "form carries the password grant and optional fields" do
      client, http = client_with(status: 200, body: { access_token: "AT", refresh_token: "RT", expires_in: 60 }.to_json)
      client.refresh(
        token_endpoint: "https://idp.example/token",
        grant: "password",
        client_id: "cid",
        client_secret: "sec",
        username: "user",
        password: "pass",
        scopes: %w[a b],
        headers: { "X-Api-Key" => "k" }
      )
      form = http.captured[:form]
      assert_equal "password", form["grant_type"]
      assert_equal "user", form["username"]
      assert_equal "pass", form["password"]
      assert_equal "cid", form["client_id"]
      assert_equal "sec", form["client_secret"]
      assert_equal "a b", form["scope"]
      assert_equal "k", http.captured[:headers]["X-Api-Key"]
      assert_equal :urlencoded, http.captured[:form_encoding]
    end

    test "form carries the Preqin username and API key as multipart" do
      client, http = client_with(status: 200, body: { access_token: "AT", refresh_token: "RT", expires_in: 60 }.to_json)
      client.refresh(
        token_endpoint: "https://api.preqin.com/connect/token",
        grant: "preqin",
        username: "preqin-user",
        api_key: "preqin-key"
      )
      form = http.captured[:form]
      assert_equal "preqin-user", form["username"]
      assert_equal "preqin-key", form["apikey"]
      refute form.key?("grant_type")
      refute form.key?("client_id")
      assert_equal :multipart, http.captured[:form_encoding]
    end

    test "form carries the Preqin refresh token as multipart" do
      client, http = client_with(status: 200, body: { access_token: "AT", refresh_token: "RT", expires_in: 60 }.to_json)
      client.refresh(
        token_endpoint: "https://api.preqin.com/connect/refresh_token",
        grant: "preqin_refresh_token",
        refresh_token: "rt-old"
      )
      assert_equal({ "refresh_token" => "rt-old" }, http.captured[:form])
      assert_equal :multipart, http.captured[:form_encoding]
    end

    test "absent refresh_token in response means no rotation" do
      client, _ = client_with(status: 200, body: { access_token: "AT", expires_in: 60 }.to_json)
      result = client.refresh(**base_args)
      assert_nil result.refresh_token
    end

    test "missing expires_in yields nil so the caller defaults it" do
      client, _ = client_with(status: 200, body: { access_token: "AT" }.to_json)
      assert_nil client.refresh(**base_args).expires_in
    end

    test "invalid_grant is unrecoverable" do
      client, _ = client_with(status: 400, body: { error: "invalid_grant" }.to_json)
      err = assert_raises(Broker::RefreshError) { client.refresh(**base_args) }
      refute err.retryable?
      assert_equal "invalid_grant", err.code
      assert_equal "invalid_grant", err.reason
    end

    test "Slack-style ok false response is unrecoverable" do
      client, _ = client_with(status: 200, body: { ok: false, error: "invalid_refresh_token" }.to_json)
      err = assert_raises(Broker::RefreshError) { client.refresh(**base_args) }
      refute err.retryable?
      assert_equal "oauth", err.stage
      assert_equal "invalid_refresh_token", err.code
    end

    test "5xx is retryable" do
      client, _ = client_with(status: 503, body: "upstream down")
      err = assert_raises(Broker::RefreshError) { client.refresh(**base_args) }
      assert err.retryable?
    end

    test "bodyless 4xx (gateway/rate-limit) is retryable" do
      client, _ = client_with(status: 429, body: "")
      err = assert_raises(Broker::RefreshError) { client.refresh(**base_args) }
      assert err.retryable?
    end

    test "malformed 2xx body is retryable parse failure" do
      client, _ = client_with(status: 200, body: "not json{")
      err = assert_raises(Broker::RefreshError) { client.refresh(**base_args) }
      assert err.retryable?
      assert_equal "parse", err.stage
    end

    test "empty access_token in 2xx is retryable" do
      client, _ = client_with(status: 200, body: { access_token: "", expires_in: 60 }.to_json)
      err = assert_raises(Broker::RefreshError) { client.refresh(**base_args) }
      assert err.retryable?
    end

    test "validates required inputs" do
      client, _ = client_with(status: 200, body: "{}")
      assert_raises(ArgumentError) { client.refresh(**base_args(refresh_token: "")) }
      assert_raises(ArgumentError) { client.refresh(**base_args(client_id: "")) }
      assert_raises(ArgumentError) do
        client.refresh(token_endpoint: "https://idp.example/token", grant: "password",
                       client_id: "cid", username: "", password: "pass")
      end
      assert_raises(ArgumentError) do
        client.refresh(token_endpoint: "https://idp.example/token", grant: "device_code",
                       client_id: "cid")
      end
      assert_raises(ArgumentError) do
        client.refresh(token_endpoint: "https://api.preqin.com/connect/token", grant: "preqin",
                       username: "user", api_key: "")
      end
    end

    test "bodyless Preqin 400 is unrecoverable so broker can fall back or die" do
      client, _ = client_with(status: 400, body: "")
      err = assert_raises(Broker::RefreshError) do
        client.refresh(token_endpoint: "https://api.preqin.com/connect/token", grant: "preqin",
                       username: "user", api_key: "key")
      end
      refute err.retryable?
      assert_equal "http_400", err.code
    end
  end
end
