require "test_helper"

class McpOauthClientTest < ActiveSupport::TestCase
  test "allowed redirect URI accepts HTTPS and plain HTTP loopback redirects" do
    assert McpOauthClient.allowed_redirect_uri?("https://claude.ai/api/mcp/auth_callback")
    assert McpOauthClient.allowed_redirect_uri?("https://example.com/callback")
    assert McpOauthClient.allowed_redirect_uri?("http://localhost:49152/callback")
    assert McpOauthClient.allowed_redirect_uri?("http://127.0.0.1:49152/callback")
    assert McpOauthClient.allowed_redirect_uri?("http://127.1.2.3:49152/callback")
    assert McpOauthClient.allowed_redirect_uri?("http://[::1]:49152/callback")

    refute McpOauthClient.allowed_redirect_uri?("http://127.evil.com/callback")
    refute McpOauthClient.allowed_redirect_uri?("http://127.0.0.1.evil.com/callback")
    refute McpOauthClient.allowed_redirect_uri?("http://localhost.evil.com/callback")
  end

  test "allowed redirect URI rejects wildcard redirects" do
    refute McpOauthClient.allowed_redirect_uri?("https://*.example.com/callback")
    refute McpOauthClient.allowed_redirect_uri?("https://example.com/*")
  end

  test "allowed redirect URI rejects fragments" do
    refute McpOauthClient.allowed_redirect_uri?("https://example.com/callback#token")
  end

  test "redirect matching rejects attacker controlled 127-looking hostnames" do
    client = McpOauthClient.create!(
      name: "Amp",
      redirect_uris: [ "http://127.0.0.1/callback" ],
      grant_types: McpOauthClient::DEFAULT_GRANT_TYPES,
      response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES,
      scopes: McpOauthClient::DEFAULT_SCOPES
    )

    refute client.redirect_uri_allowed?("http://127.evil.com/callback")
    refute client.redirect_uri_allowed?("http://127.0.0.1.evil.com/callback")
  end
end
