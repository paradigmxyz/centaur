require "test_helper"

class SpaRequestTest < ActiveSupport::TestCase
  test "serves SPA paths and keeps protocol, API, and login namespaces out" do
    %w[/ /console /console/principals /console/roles/role_1 /whatever].each do |path|
      assert SpaRequest.spa_path?(path), path
    end
    %w[
      /api /api/v1/me /mcp /mcp/oauth /mcp/oauth/revoke /mcp.html /oauth /oauth/x/y /oauth.html
      /auth /auth/x /auth.html /login /login.html /logout /pending /pending.html /.well-known/x /up
    ].each do |path|
      assert_not SpaRequest.spa_path?(path), path
    end
    assert SpaRequest.spa_path?("/authors"), "only exact namespace segments are excluded"
  end
end
