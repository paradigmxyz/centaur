require "test_helper"

class SpaControllerTest < ActionDispatch::IntegrationTest
  HTML = { "Accept" => "text/html,application/xhtml+xml" }.freeze

  setup do
    @dir = Dir.mktmpdir
    @index = File.join(@dir, "index.html")
    File.write(@index, "<!doctype html><div id=\"root\"></div>")
    @previous_index_path = SpaController.index_path
    SpaController.index_path = @index
  end

  teardown do
    SpaController.index_path = @previous_index_path
    FileUtils.remove_entry(@dir)
  end

  test "HTML navigations to SPA paths serve the built index" do
    [ "/console/spa-only-page", "/console/spa-only/nested/prn_123" ].each do |path|
      get path, headers: HTML
      assert_response :ok, path
      assert_includes response.body, "<div id=\"root\">"
      assert_equal "no-cache", response.headers["Cache-Control"]
      assert_match %r{text/html}, response.media_type
    end
  end

  test "path helpers used by redirects point at the SPA pages" do
    app = oauth_apps(:acme_google)
    assert_equal "/console/principals", console_principals_path
    assert_equal "/console/integrations", console_integrations_path
    assert_equal "/console/oauth_apps", console_oauth_apps_path
    assert_equal "/console/oauth_apps/#{app.oid}", console_oauth_app_path(app.oid)
  end

  test "a signed-out deep link is remembered for the post-login redirect" do
    get "/console/spa-only-page?q=1", headers: HTML
    console_password_login(users(:acme_admin).email)
    assert_redirected_to "/console/spa-only-page?q=1"
  end

  test "only allowlisted deep links are remembered" do
    get "/console/a%2e%2e/b", headers: HTML
    assert_nil session[:return_to]
  end

  test "non-HTML requests to SPA paths keep the JSON 404" do
    get "/console/spa-only-page", headers: { "Accept" => "application/json" }
    assert_response :not_found
    assert_equal({ "error" => { "message" => "not found" } }, JSON.parse(response.body))

    get "/console/spa-only-page", headers: { "Accept" => "*/*" }
    assert_response :not_found
  end

  test "non-GET requests to SPA paths keep the JSON 404" do
    post "/console/spa-only-page", headers: HTML
    assert_response :not_found
  end

  test "unclaimed HTML navigations outside /console serve the SPA not-found page" do
    get "/elsewhere", headers: HTML
    assert_response :ok
  end

  test "HTML requests to API, framework, and protocol prefixes keep the JSON 404" do
    [ "/api/v1/nope", "/api", "/rails/nope", "/spa/assets/missing.js", "/.well-known/nope", "/mcp/oauth/token",
      "/mcp", "/mcp/nope", "/mcp/oauth/nope", "/mcp/oauth/consent", "/oauth", "/oauth/result", "/oauth/a/b/c",
      "/auth", "/auth/nope", "/auth/google/start/extra", "/login/nope", "/logout", "/pending/nope" ].each do |path|
      get path, headers: HTML
      assert_response :not_found, path
      assert_equal "not found", JSON.parse(response.body).dig("error", "message"), path
    end
  end

  test "Rails protocol routes take precedence over the SPA" do
    # OAuth protocol endpoints stay Rails routes.
    get "/oauth/unknown-app/start", headers: HTML
    assert_response :redirect
    # The login and pending pages are server-rendered, not SPA routes.
    get "/login", headers: HTML
    assert_response :ok
    assert_not_includes response.body, "<div id=\"root\">"
    get "/pending", headers: HTML
    assert_redirected_to "/login"
  end

  test "an unbuilt SPA explains how to build it" do
    SpaController.index_path = File.join(@dir, "missing.html")
    get "/console/spa-only-page", headers: HTML
    assert_response :service_unavailable
    assert_includes response.body, "pnpm -C services/console/web build"
  end
end
