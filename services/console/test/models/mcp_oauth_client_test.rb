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

  test "registration refuses executable redirect URI schemes however they are spelled" do
    [
      "javascript://x/%0aalert(document.domain)//",
      "JavaScript://x/%0aalert(1)",
      "JAVASCRIPT:alert(1)",
      " javascript:alert(1)",
      "\u0000javascript:alert(1)",
      "\u0001\tjavascript:alert(1)",
      "java\tscript:alert(1)",
      "vbscript:msgbox(1)",
      "LiveScript:alert(1)",
      "data:text/html;base64,PHNjcmlwdD4=",
      "Data:text/plain,x"
    ].each do |uri|
      client = build_client(redirect_uris: [ uri ])
      assert_not client.valid?, uri.inspect
      assert client.errors[:redirect_uris].any?, uri.inspect
    end
  end

  test "registration keeps the origin/main redirect URI rules for every other scheme" do
    %w[
      https://claude.ai/api/mcp/auth_callback
      http://127.0.0.1:49152/callback
      vscode://vscode.github-authentication/did-authenticate
      cursor://anysphere.cursor-mcp/oauth/callback
      chrome-extension://abcdefghijklmnopabcdefghijklmnop/callback
      com.example.app:/oauth2redirect
      javascriptx://callback
      my-data://callback
    ].each { |uri| assert build_client(redirect_uris: [ uri ]).valid?, uri }
  end

  test "an existing registration is never newly refused at authorize time" do
    %w[
      chrome-extension://abcdefghijklmnopabcdefghijklmnop/callback
      ws://localhost/callback
      file:///tmp/callback
    ].each do |stored|
      client = build_client(redirect_uris: [ "https://example.com/cb" ])
      client.save!
      client.update_column(:redirect_uris, [ stored ])
      assert client.reload.redirect_uri_allowed?(stored), stored
      assert client.valid?, "#{stored} must stay valid on update"
    end
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

  private

  def build_client(redirect_uris:)
    McpOauthClient.new(
      name: "Client", redirect_uris: redirect_uris,
      grant_types: McpOauthClient::DEFAULT_GRANT_TYPES, response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES,
      scopes: McpOauthClient::DEFAULT_SCOPES
    )
  end
end
