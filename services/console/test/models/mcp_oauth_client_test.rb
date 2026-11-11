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

  test "allowed redirect URI accepts private-use scheme redirects" do
    assert McpOauthClient.allowed_redirect_uri?("cursor://anysphere.cursor-mcp/oauth/callback")
    assert McpOauthClient.allowed_redirect_uri?("vscode://mcp/callback")
    assert McpOauthClient.allowed_redirect_uri?("com.example.app:/oauth2redirect")

    refute McpOauthClient.allowed_redirect_uri?("/oauth/callback")
    refute McpOauthClient.allowed_redirect_uri?("callback")
  end

  test "allowed redirect URI rejects wildcard redirects" do
    refute McpOauthClient.allowed_redirect_uri?("https://*.example.com/callback")
    refute McpOauthClient.allowed_redirect_uri?("https://example.com/*")
    refute McpOauthClient.allowed_redirect_uri?("cursor://anysphere.cursor-mcp/*")
  end

  test "allowed redirect URI rejects fragments" do
    refute McpOauthClient.allowed_redirect_uri?("https://example.com/callback#token")
    refute McpOauthClient.allowed_redirect_uri?("cursor://anysphere.cursor-mcp/oauth/callback#token")
  end

  test "rejects executable and browser-reserved schemes however they are spelled" do
    [
      "javascript://x/%0aalert(document.domain)//",
      "JavaScript://x/%0aalert(1)",
      "JAVASCRIPT:alert(1)",
      " javascript://x/%0aalert(1)",
      "java\tscript://x/%0aalert(1)",
      "javascript\n://x/alert(1)",
      "\u0000javascript://x",
      "vbscript://x/msgbox",
      "livescript://x",
      "data://text/html,<script>alert(1)</script>",
      "data:text/html;base64,PHNjcmlwdD4=",
      "blob://https://example.com/uuid",
      "file:///etc/passwd",
      "filesystem://https://example.com/temporary/x",
      "about://blank",
      "view-source://https://example.com",
      "chrome-extension://abc/x",
      "ws://localhost/x",
      "https:\\\\evil.example/cb",
      "1abc://x"
    ].each do |uri|
      refute McpOauthClient.allowed_redirect_uri?(uri), uri.inspect
    end
  end

  test "keeps legitimate https, loopback http, and native custom-scheme callbacks" do
    %w[
      https://claude.ai/api/mcp/auth_callback
      http://127.0.0.1:49152/callback
      vscode://vscode.github-authentication/did-authenticate
      cursor://anysphere.cursor-mcp/oauth/callback
      com.example.app:/oauth2redirect
    ].each { |uri| assert McpOauthClient.allowed_redirect_uri?(uri), uri }
  end

  test "registration refuses a javascript redirect URI" do
    client = McpOauthClient.new(
      name: "Attacker", redirect_uris: [ "javascript://x/%0aalert(document.domain)//" ],
      grant_types: McpOauthClient::DEFAULT_GRANT_TYPES, response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES,
      scopes: McpOauthClient::DEFAULT_SCOPES
    )
    assert_not client.valid?
    assert_match "not an allowed public-client redirect URI", client.errors[:redirect_uris].join
  end

  test "an existing registration with a forbidden scheme never matches" do
    client = McpOauthClient.create!(
      name: "Legacy", redirect_uris: [ "https://example.com/cb" ],
      grant_types: McpOauthClient::DEFAULT_GRANT_TYPES, response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES,
      scopes: McpOauthClient::DEFAULT_SCOPES
    )
    bad = "javascript://x/%0aalert(document.domain)//"
    client.update_column(:redirect_uris, [ bad ])
    assert_not client.redirect_uri_allowed?(bad)
  end

  test "private-use scheme redirects match by exact string equality only" do
    client = McpOauthClient.create!(
      name: "Cursor",
      redirect_uris: [ "cursor://anysphere.cursor-mcp/oauth/callback" ],
      grant_types: McpOauthClient::DEFAULT_GRANT_TYPES,
      response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES,
      scopes: McpOauthClient::DEFAULT_SCOPES
    )

    assert client.redirect_uri_allowed?("cursor://anysphere.cursor-mcp/oauth/callback")
    refute client.redirect_uri_allowed?("cursor://anysphere.cursor-mcp/oauth/callback/extra")
    refute client.redirect_uri_allowed?("cursor://evil/oauth/callback")
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
