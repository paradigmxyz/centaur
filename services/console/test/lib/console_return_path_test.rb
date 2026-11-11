require "test_helper"

class ConsoleReturnPathTest < ActiveSupport::TestCase
  test "allows the root, console SPA paths, and session-gated GET entry points" do
    {
      "/" => "/",
      "/console" => "/console",
      "/console/credentials?kind=oauth" => "/console/credentials?kind=oauth",
      "/console/principals/prn_123#grants" => "/console/principals/prn_123",
      "/mcp/oauth/authorize?client_id=moc_1&state=x" => "/mcp/oauth/authorize?client_id=moc_1&state=x",
      "/mcp/oauth/authorize.html?client_id=moc_1&redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2Fcb&scope=a+b" =>
        "/mcp/oauth/authorize.html?client_id=moc_1&redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2Fcb&scope=a+b",
      "/oauth/google/start?scopes=a+b" => "/oauth/google/start?scopes=a+b",
      "/oauth/google~team/start" => "/oauth/google~team/start",
      "/oauth/acme.google_v2-x/start" => "/oauth/acme.google_v2-x/start",
      "/launch?target=web%2Bcentaur%3A%2F%2Fconsole" => "/launch?target=web%2Bcentaur%3A%2F%2Fconsole",
      # Opaque OAuth state may carry an unencoded backslash, as origin/main accepted.
      "/mcp/oauth/authorize?client_id=moc_1&state=opaque\\client-state" =>
        "/mcp/oauth/authorize?client_id=moc_1&state=opaque\\client-state"
    }.each do |raw, expected|
      assert_equal expected, ConsoleReturnPath.safe(raw), raw
    end
  end

  test "rejects off-origin, protocol-relative, and smuggled targets" do
    [
      nil, "", "console", "https://evil.example/console", "//evil.example/console",
      "///evil.example", "/\\evil.example", "/\tevil", "/console\n//evil", "javascript:alert(1)",
      "http:/console", "/console/../api/v1/me", "/console/./x/..", "/console/%2e%2e/api",
      "/console/%2E%2E/api", "/consolex", "/api/v1/me", "/logout", "/login", "/pending",
      "/oauth/google/callback?code=x", "/auth/google/callback", "/mcp/oauth/token",
      "/mcp/oauth/authorize.json", "/mcp/oauth/authorizex", "/mcp/oauth/authorize/x",
      "/oauth/../console", "/oauth/../start", "/oauth/./start", "/oauth/%2e%2e/start",
      "/oauth/a%2Fb/start", "/oauth/a b/start", "/oauth/a@b/start", "/#{'a' * 5000}",
      "/mcp/oauth/authorize?state=a\nLocation:%20x", "/mcp/oauth/authorize?state=a b", "/\\evil.example?x=\\"
    ].each do |raw|
      assert_nil ConsoleReturnPath.safe(raw), raw.inspect
    end
  end

  test "accepts every slug an OAuth app can have" do
    %w[google google~team a.b_c-d ~ x~].each do |slug|
      assert OauthApp::URL_SAFE_FORMAT.match?(slug)
      assert_equal "/oauth/#{slug}/start", ConsoleReturnPath.safe("/oauth/#{slug}/start"), slug
    end
  end
end
