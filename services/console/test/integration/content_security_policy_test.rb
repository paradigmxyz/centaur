require "test_helper"

# The enforcing Content-Security-Policy (config/initializers/content_security_policy.rb
# and ServerRenderedPagePolicy). The exact header strings are pinned so any
# loosening is a deliberate, reviewed change.
class ContentSecurityPolicyTest < ActionDispatch::IntegrationTest
  HTML = { "Accept" => "text/html,application/xhtml+xml" }.freeze

  SPA_POLICY = [
    "default-src 'self'",
    "script-src 'self'",
    "style-src 'self'",
    "style-src-elem 'self' 'unsafe-inline'",
    "img-src 'self' data:",
    "font-src 'self'",
    "connect-src 'self'",
    "worker-src 'self'",
    "manifest-src 'self'",
    "object-src 'none'",
    "base-uri 'none'",
    "frame-ancestors 'none'"
  ].join("; ").freeze

  SERVER_PAGE_POLICY = [
    "default-src 'self'",
    "script-src 'none'",
    "style-src 'self'",
    "img-src 'self' data:",
    "font-src 'self'",
    "connect-src 'self'",
    "worker-src 'self'",
    "manifest-src 'self'",
    "object-src 'none'",
    "base-uri 'none'",
    "frame-ancestors 'none'"
  ].join("; ").freeze

  WEB_ROOT = Rails.root.join("web")
  INLINE_SCRIPT = /<script\b(?![^>]*\bsrc=)[^>]*>/i

  setup do
    @saved_mcp_url = ENV["CENTAUR_MCP_PUBLIC_URL"]
    ENV["CENTAUR_MCP_PUBLIC_URL"] = "http://localhost:3000/mcp"
  end

  teardown do
    @saved_mcp_url.nil? ? ENV.delete("CENTAUR_MCP_PUBLIC_URL") : ENV["CENTAUR_MCP_PUBLIC_URL"] = @saved_mcp_url
  end

  test "the SPA shell is served through Rails with the strict SPA policy" do
    with_spa_index("<!doctype html><div id=\"root\"></div>") do
      [ "/", "/console/principals", "/console/skills/skl_123" ].each do |path|
        get path, headers: HTML
        assert_response :ok, path
        assert_includes response.body, "<div id=\"root\">"
        assert_csp SPA_POLICY, path
      end
    end
  end

  test "the SPA shell keeps the policy when the build is missing" do
    with_spa_index("/nonexistent/index.html", write: false) do
      get "/console/principals", headers: HTML
      assert_response :service_unavailable
      assert_csp SPA_POLICY
    end
  end

  test "the login and pending pages allow no script and no form-action" do
    get "/login"
    assert_response :ok
    assert_csp SERVER_PAGE_POLICY

    post "/login", params: { email: users(:acme_admin).email, password: "wrong" }
    assert_response :unprocessable_entity
    assert_csp SERVER_PAGE_POLICY

    console_password_login(users(:pending_user).email)
    get "/pending"
    assert_response :ok
    assert_csp SERVER_PAGE_POLICY
  end

  test "the MCP consent page allows no script and sets no form-action" do
    client = create_mcp_client("http://127.0.0.1:49152/callback")
    console_password_login(users(:acme_admin).email)

    get "/mcp/oauth/authorize", params: authorize_params(client, "http://127.0.0.1:49152/callback")
    assert_response :ok
    assert_select "form[action=?]", "/mcp/oauth/authorize"
    assert_csp SERVER_PAGE_POLICY
  end

  # Browsers apply form-action to the redirect that follows a form post, so
  # the consent decision's redirect to a loopback or custom-scheme
  # redirect_uri must never be covered by one.
  test "consent approve and deny redirect to loopback and custom-scheme URIs under a policy without form-action" do
    console_password_login(users(:acme_admin).email)
    [ "http://127.0.0.1:49152/callback", "cursor://anysphere.cursor-mcp/oauth/callback" ].each do |redirect|
      client = create_mcp_client(redirect)
      params = authorize_params(client, redirect)

      get "/mcp/oauth/authorize", params: params
      assert_csp SERVER_PAGE_POLICY, redirect

      post "/mcp/oauth/authorize", params: params.merge(decision: "approve")
      assert_response :redirect
      assert response.location.start_with?(redirect), response.location
      assert_includes response.location, "code="
      assert_csp SERVER_PAGE_POLICY, redirect

      post "/mcp/oauth/authorize", params: params.merge(decision: "deny")
      assert_response :redirect
      assert response.location.start_with?(redirect), response.location
      assert_includes response.location, "error=access_denied"
      assert_csp SERVER_PAGE_POLICY, redirect
    end
  end

  test "the server-rendered policy applies even when the login gate halts first" do
    get "/mcp/oauth/authorize", params: { client_id: "moc_unknown" }
    assert_redirected_to "/login"
    assert_csp SERVER_PAGE_POLICY

    get oauth_start_url(slug: "google")
    assert_redirected_to "/login"
    assert_csp SERVER_PAGE_POLICY
  end

  test "the broker OAuth result page allows no script" do
    console_password_login(users(:acme_admin).email)
    get oauth_start_url(slug: "nope")
    assert_response :not_found
    assert_match "Unknown integration", response.body
    assert_csp SERVER_PAGE_POLICY
  end

  test "JSON, metadata, health, and PWA responses are unaffected" do
    get "/api/v1/me"
    assert_response :unauthorized
    assert_equal "application/json", response.media_type

    get "/.well-known/oauth-authorization-server"
    assert_response :ok
    assert JSON.parse(response.body).key?("authorization_endpoint")

    post "/mcp/oauth/register", params: { client_name: "CSP", redirect_uris: [ "cursor://csp/callback" ] }, as: :json
    assert_response :created

    post "/mcp/oauth/token", params: { grant_type: "nope" }
    assert_response :bad_request
    assert_equal "unsupported_grant_type", JSON.parse(response.body).fetch("error")

    get "/up"
    assert_response :ok

    get "/manifest.json"
    assert_response :ok
    get "/service-worker.js"
    assert_response :ok
  end

  test "web/index.html has no inline script; the pre-paint theme script is an external file" do
    source = WEB_ROOT.join("index.html").read
    assert_no_match INLINE_SCRIPT, source.gsub(/<script type="module" src="[^"]+"><\/script>/, "")
    assert_no_match(/\son[a-z]+\s*=/i, source, "no inline event handlers")
    assert WEB_ROOT.join("theme-boot.js").file?
    assert_includes WEB_ROOT.join("vite.config.ts").read, "theme-boot.js"
  end

  test "the built SPA index has no inline script and loads the theme script from /spa/assets" do
    built = Rails.public_path.join("spa", "index.html")
    skip "SPA not built (pnpm -C web build)" unless built.file?

    html = built.read
    assert_no_match INLINE_SCRIPT, html
    assert_no_match(/\son[a-z]+\s*=/i, html)
    theme = html[%r{<script src="(/spa/assets/theme-boot-[^"]+\.js)"></script>}, 1]
    assert theme, "theme-boot script tag"
    assert Rails.public_path.join(theme.delete_prefix("/")).file?
    assert html.index(theme) < html.index("</head>"), "theme script runs from <head>, before first paint"
  end

  private

  def assert_csp(expected, message = nil)
    assert_nil response.headers["Content-Security-Policy-Report-Only"], message
    assert_equal expected, response.headers["Content-Security-Policy"], message
    assert_not_includes response.headers["Content-Security-Policy"].to_s, "form-action", message
  end

  def with_spa_index(content, write: true)
    dir = Dir.mktmpdir
    path = write ? File.join(dir, "index.html").tap { |p| File.write(p, content) } : content
    previous = SpaController.index_path
    SpaController.index_path = path
    yield
  ensure
    SpaController.index_path = previous
    FileUtils.remove_entry(dir)
  end

  def create_mcp_client(redirect)
    McpOauthClient.create!(
      name: "CSP client",
      redirect_uris: [ redirect ],
      grant_types: McpOauthClient::DEFAULT_GRANT_TYPES,
      response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES,
      scopes: McpOauthClient::DEFAULT_SCOPES
    )
  end

  def authorize_params(client, redirect)
    {
      response_type: "code",
      client_id: client.public_client_id,
      redirect_uri: redirect,
      scope: "mcp:tools",
      state: "state-csp",
      resource: "http://localhost:3000/mcp",
      code_challenge: Base64.urlsafe_encode64(Digest::SHA256.digest("csp-verifier"), padding: false),
      code_challenge_method: "S256"
    }
  end
end
