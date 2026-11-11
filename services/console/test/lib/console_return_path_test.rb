require "test_helper"

class ConsoleReturnPathTest < ActiveSupport::TestCase
  test "allows the root, console SPA paths, and session-gated GET entry points" do
    {
      "/" => "/",
      "/console" => "/console",
      "/console/credentials?kind=oauth" => "/console/credentials?kind=oauth",
      "/console/principals/prn_123#grants" => "/console/principals/prn_123",
      "/mcp/oauth/authorize?client_id=moc_1&state=x" => "/mcp/oauth/authorize?client_id=moc_1&state=x",
      "/oauth/google/start?scopes=a+b" => "/oauth/google/start?scopes=a+b",
      "/launch?target=web%2Bcentaur%3A%2F%2Fconsole" => "/launch?target=web%2Bcentaur%3A%2F%2Fconsole"
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
      "/oauth/../console", "/#{'a' * 5000}"
    ].each do |raw|
      assert_nil ConsoleReturnPath.safe(raw), raw.inspect
    end
  end
end
