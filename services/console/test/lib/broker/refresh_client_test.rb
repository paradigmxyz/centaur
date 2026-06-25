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

    def base_request(**overrides)
      {
        url: "https://idp.example/token",
        form: {
          "grant_type" => "refresh_token",
          "refresh_token" => "rt-old",
          "client_id" => "cid"
        }
      }.merge(overrides)
    end

    test "successful refresh parses the RFC 6749 body" do
      client, _ = client_with(status: 200, body: { access_token: "AT", refresh_token: "RT", expires_in: 3600 }.to_json)
      result = client.refresh(**base_request)
      assert_equal "AT", result.access_token
      assert_equal "RT", result.refresh_token
      assert_equal 3600, result.expires_in
    end

    test "posts supplied URL-encoded form and headers" do
      client, http = client_with(status: 200, body: { access_token: "AT", expires_in: 60 }.to_json)
      form = {
        "grant_type" => "refresh_token",
        "refresh_token" => "rt-old",
        "client_id" => "cid",
        "client_secret" => "sec",
        "scope" => "a b"
      }
      client.refresh(**base_request(form: form, headers: { "X-Api-Key" => "k" }))
      assert_equal "https://idp.example/token", http.captured[:url]
      assert_equal form, http.captured[:form]
      assert_equal "k", http.captured[:headers]["X-Api-Key"]
      assert_equal :urlencoded, http.captured[:form_encoding]
    end

    test "posts supplied multipart form" do
      client, http = client_with(status: 200, body: { access_token: "AT", expires_in: 60 }.to_json)
      form = { "username" => "user", "apikey" => "key" }
      client.refresh(**base_request(form: form, form_encoding: :multipart))
      assert_equal form, http.captured[:form]
      assert_equal :multipart, http.captured[:form_encoding]
    end

    test "absent refresh_token in response means no rotation" do
      client, _ = client_with(status: 200, body: { access_token: "AT", expires_in: 60 }.to_json)
      result = client.refresh(**base_request)
      assert_nil result.refresh_token
    end

    test "missing expires_in yields nil so the caller defaults it" do
      client, _ = client_with(status: 200, body: { access_token: "AT" }.to_json)
      assert_nil client.refresh(**base_request).expires_in
    end

    test "invalid_grant is unrecoverable" do
      client, _ = client_with(status: 400, body: { error: "invalid_grant" }.to_json)
      err = assert_raises(Broker::RefreshError) { client.refresh(**base_request) }
      refute err.retryable?
      assert_equal "invalid_grant", err.code
      assert_equal "invalid_grant", err.reason
    end

    test "Slack-style ok false response is unrecoverable" do
      client, _ = client_with(status: 200, body: { ok: false, error: "invalid_refresh_token" }.to_json)
      err = assert_raises(Broker::RefreshError) { client.refresh(**base_request) }
      refute err.retryable?
      assert_equal "oauth", err.stage
      assert_equal "invalid_refresh_token", err.code
    end

    test "5xx is retryable" do
      client, _ = client_with(status: 503, body: "upstream down")
      err = assert_raises(Broker::RefreshError) { client.refresh(**base_request) }
      assert err.retryable?
    end

    test "bodyless 4xx is retryable by default" do
      client, _ = client_with(status: 429, body: "")
      err = assert_raises(Broker::RefreshError) { client.refresh(**base_request) }
      assert err.retryable?
    end

    test "bodyless 4xx can be strict and unrecoverable" do
      client, _ = client_with(status: 400, body: "")
      err = assert_raises(Broker::RefreshError) { client.refresh(**base_request(strict_4xx: true)) }
      refute err.retryable?
      assert_equal "http_400", err.code
    end

    test "malformed 2xx body is retryable parse failure" do
      client, _ = client_with(status: 200, body: "not json{")
      err = assert_raises(Broker::RefreshError) { client.refresh(**base_request) }
      assert err.retryable?
      assert_equal "parse", err.stage
    end

    test "empty access_token in 2xx is retryable" do
      client, _ = client_with(status: 200, body: { access_token: "", expires_in: 60 }.to_json)
      err = assert_raises(Broker::RefreshError) { client.refresh(**base_request) }
      assert err.retryable?
    end

    test "validates request inputs" do
      client, _ = client_with(status: 200, body: "{}")
      assert_raises(ArgumentError) { client.refresh(**base_request(url: "")) }
      assert_raises(ArgumentError) { client.refresh(**base_request(form: nil)) }
      assert_raises(ArgumentError) { client.refresh(**base_request(form_encoding: :xml)) }
    end
  end
end
