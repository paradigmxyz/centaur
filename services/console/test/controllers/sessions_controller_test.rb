require "test_helper"

# The server-rendered login and pending pages and their form endpoints
# (GET/POST /login, DELETE /logout, GET /pending). During a rolling deploy a
# pod on either version must accept the other's forms.
class SessionsControllerTest < ActionDispatch::IntegrationTest
  HTML = { "Accept" => "text/html,application/xhtml+xml" }.freeze

  # The previous release's login form, verbatim apart from the CSS classes.
  # Rendered through this app's helpers to prove a form an old pod served
  # posts successfully to a new one.
  ORIGIN_MAIN_LOGIN_FORM = <<~ERB.freeze
    <%= form_with url: login_path, method: :post do |f| %>
      <%= f.label :email %>
      <%= f.email_field :email, placeholder: "you@example.com", autocomplete: "username", autofocus: true %>
      <%= f.label :password %>
      <%= f.password_field :password, autocomplete: "current-password" %>
      <%= f.submit "Sign in" %>
    <% end %>
  ERB

  setup { @operator = users(:acme_admin) }

  teardown do
    ENV.delete("CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED")
    %w[GOOGLE SLACK].each do |provider|
      ENV.delete("CENTAUR_CONSOLE_#{provider}_CLIENT_ID")
      ENV.delete("CENTAUR_CONSOLE_#{provider}_CLIENT_SECRET")
    end
  end

  def with_forgery_protection
    previous = ActionController::Base.allow_forgery_protection
    ActionController::Base.allow_forgery_protection = true
    yield
  ensure
    ActionController::Base.allow_forgery_protection = previous
  end

  def form_fields(selector)
    form = Nokogiri::HTML(response.body).at_css(selector) || flunk("no #{selector} in #{response.body}")
    form.css("input[name]").to_h { |input| [ input["name"], input["value"].to_s ] }
  end

  def sign_in_form(email, password = ConsoleApiAuthHelper::TEST_PASSWORD)
    get login_path
    form_fields("form[action='/login']").merge("email" => email, "password" => password)
  end

  test "GET new renders the login form without JavaScript" do
    get login_url
    assert_response :ok
    assert_select "title", "Sign In · Centaur Console"
    assert_select "h1", "Operator console sign in."
    assert_select "form[action=?][method=post]", login_path do
      assert_select "input[type=email][name=email][autocomplete=username][autofocus]"
      assert_select "input[type=password][name=password][autocomplete=current-password]"
      assert_select "input[type=submit][value=?]", "Sign in"
    end
    assert_select "link[rel=stylesheet][href^=?]", "/oauth-page/oauth.css"
    assert_select "script", count: 0
    assert_not_includes response.body, "<div id=\"root\">"
  end

  test "GET new hides the password form when password login is disabled" do
    ENV["CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED"] = "false"
    get login_url
    assert_response :ok
    assert_select "form[action=?]", login_path, count: 0
    assert_select "input[name=?]", "email", count: 0
  end

  test "GET new offers a button for each configured SSO provider only" do
    ENV["CENTAUR_CONSOLE_GOOGLE_CLIENT_ID"] = "google-login-client-id"
    ENV["CENTAUR_CONSOLE_GOOGLE_CLIENT_SECRET"] = "google-login-secret"
    # Slack without a secret is not configured, so it must not appear.
    ENV["CENTAUR_CONSOLE_SLACK_CLIENT_ID"] = "slack-login-client-id"

    get login_url
    assert_select "a[href=?]", auth_start_path(provider: "google"), text: "Continue with Google"
    assert_select "a[href=?]", auth_start_path(provider: "slack"), count: 0
    assert_select ".oauth-divider", text: "or sign in with email"
    assert_select "form[action=?]", login_path
    assert_not_includes response.body, "google-login-secret"

    ENV["CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED"] = "false"
    get login_url
    assert_select "a[href=?]", auth_start_path(provider: "google")
    assert_select ".oauth-divider", count: 0
    assert_select "form[action=?]", login_path, count: 0
  end

  test "a signed-in active user visiting the login page goes to the console" do
    sign_in_console(@operator)
    get login_url
    assert_redirected_to console_principals_path

    get login_url(return_to: "/console/credentials?kind=oauth")
    assert_redirected_to "/console/credentials?kind=oauth"

    sign_in_console(users(:member_user))
    get login_url
    assert_redirected_to console_integrations_path
  end

  test "a signed-in user is never sent off-origin from the login page" do
    sign_in_console(@operator)
    [ "https://evil.example/", "//evil.example", "/\\evil.example", "/console/../api/v1/me",
      "/api/v1/principals", "javascript:alert(1)" ].each do |target|
      get login_url(return_to: target)
      assert_redirected_to console_principals_path, target
    end
  end

  test "a pending user visiting the login page sees the form" do
    sign_in_console(users(:pending_user))
    get login_url
    assert_response :ok
    assert_select "form[action=?]", login_path
  end

  test "the login page renders the flash once" do
    sign_in_console(@operator)
    delete logout_url
    follow_redirect!
    assert_select "[role=status]", text: "Signed out."
    get login_url
    assert_select "[role=status]", count: 0
  end

  test "valid credentials sign in and redirect to the console" do
    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_redirected_to console_principals_path
    assert_equal @operator.id, session[:user_id]
    assert_equal "Signed in as #{@operator.email}.", flash[:notice]

    # The SPA shows the notice it hands off through /api/v1/me.
    get api_v1_console_me_path
    assert_equal "Signed in as #{@operator.email}.", json_body.dig("data", "flash", "notice")
  end

  test "login redirects to the protected console URL the user first requested" do
    # Signed-out deep links are served the SPA shell, which remembers the
    # destination; the SPA then sends the browser to /login.
    get "/console/credentials?kind=oauth", headers: HTML
    assert_equal "/console/credentials?kind=oauth", session[:return_to]

    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_redirected_to "/console/credentials?kind=oauth"
    assert_equal @operator.id, session[:user_id]
    assert_nil session[:return_to]
  end

  test "the login page remembers an allowlisted return_to for the next sign-in" do
    get login_url(return_to: "/console/credentials?kind=oauth")
    assert_response :ok
    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_redirected_to "/console/credentials?kind=oauth"
  end

  test "the login page ignores return_to values outside the allowlist" do
    [ "https://evil.example/", "//evil.example/x", "/\\evil.example", "/logout", "/api/v1/me",
      "/console/%2e%2e/api", "/console/../oauth/x/callback" ].each do |target|
      reset!
      get login_url(return_to: target)
      assert_nil session[:return_to], target
      post login_url, params: { email: @operator.email, password: "password123456" }
      assert_redirected_to console_principals_path, target
    end
  end

  test "a return_to on the login page does not replace one remembered earlier when absent" do
    get "/console/credentials?kind=oauth", headers: HTML
    get login_url
    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_redirected_to "/console/credentials?kind=oauth"
  end

  test "login returns to the MCP authorize request with its full query" do
    query = "response_type=code&client_id=moc_x&redirect_uri=http%3A%2F%2F127.0.0.1%3A49152%2Fcallback" \
            "&scope=mcp%3Atools&state=s%2B1&code_challenge=abc&code_challenge_method=S256"
    [ "/mcp/oauth/authorize", "/mcp/oauth/authorize.html" ].each do |path|
      reset!
      get "#{path}?#{query}"
      assert_redirected_to login_path, path
      post login_url, params: { email: @operator.email, password: "password123456" }
      assert_redirected_to "#{path}?#{query}", path
    end
  end

  test "login returns to an MCP authorize request whose state carries an unencoded backslash" do
    # Clients that send opaque state without encoding "\\" must keep working.
    query = "response_type=code&client_id=moc_x&redirect_uri=http%3A%2F%2F127.0.0.1%3A49152%2Fcallback" \
            "&scope=mcp%3Atools&state=opaque\\client-state&code_challenge=abc&code_challenge_method=S256"
    get "/mcp/oauth/authorize?#{query}"
    assert_redirected_to login_path
    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_equal "http://www.example.com/mcp/oauth/authorize?#{query}", response.location
  end

  test "unsafe methods do not replace the remembered login destination" do
    get "/console/credentials?kind=oauth", headers: HTML
    assert_equal "/console/credentials?kind=oauth", session[:return_to]

    post "/mcp/oauth/authorize", params: { client_id: "x" }
    assert_redirected_to login_path
    assert_equal "/console/credentials?kind=oauth", session[:return_to]
  end

  test "login resets the session (no fixation) but keeps the return path" do
    get "/console/credentials?kind=oauth", headers: HTML
    old_session_id = session.id.to_s
    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_not_equal old_session_id, session.id.to_s
    assert_redirected_to "/console/credentials?kind=oauth"
  end

  test "password login rejects credentials when disabled" do
    ENV["CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED"] = "false"
    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_response :not_found
    assert_nil session[:user_id]
    assert_select "[role=alert]", text: "Email and password sign in is disabled."
  end

  test "a non-admin lands on integrations after login" do
    member = users(:member_user)
    post login_url, params: { email: member.email, password: "password123456" }
    assert_redirected_to console_integrations_path
    assert_equal member.id, session[:user_id]
  end

  test "email match is case-insensitive and trims whitespace" do
    post login_url, params: { email: "  #{@operator.email.upcase} ", password: "password123456" }
    assert_equal @operator.id, session[:user_id]
  end

  test "invalid password re-renders the form without a session" do
    post login_url, params: { email: @operator.email, password: "wrong" }
    assert_response :unprocessable_entity
    assert_nil session[:user_id]
    assert_select "[role=alert]", text: "Invalid email or password."
    assert_select "input[name=password][value]", count: 0
    assert_not_includes response.body, "wrong\""
  end

  test "an unknown email or missing fields are rejected like a bad password" do
    post login_url, params: { email: "nobody@example.test", password: "password123456" }
    assert_response :unprocessable_entity
    assert_select "[role=alert]", text: "Invalid email or password."

    post login_url
    assert_response :unprocessable_entity
    assert_nil session[:user_id]
  end

  test "a pending user is signed in but routed to the holding page" do
    pending = users(:pending_user)
    post login_url, params: { email: pending.email, password: "password123456" }
    assert_redirected_to pending_path
    assert_equal pending.id, session[:user_id]
    follow_redirect!
    assert_response :ok
    assert_select "[role=status]", text: "Your account is awaiting approval."
  end

  test "a disabled user cannot sign in" do
    disabled = users(:disabled_user)
    post login_url, params: { email: disabled.email, password: "password123456" }
    assert_response :unprocessable_entity
    assert_nil session[:user_id]
    assert_select "[role=alert]", text: "Your account is disabled."
  end

  test "the login form carries an authenticity token and POST /login requires it" do
    with_forgery_protection do
      fields = sign_in_form(@operator.email)
      assert fields["authenticity_token"].present?

      post login_url, params: fields.except("authenticity_token")
      assert_response :unprocessable_entity
      assert_nil session[:user_id]

      post login_url, params: fields.merge("authenticity_token" => "not-a-token")
      assert_response :unprocessable_entity
      assert_nil session[:user_id]

      post login_url, params: fields
      assert_redirected_to console_principals_path
      assert_equal @operator.id, session[:user_id]
    end
  end

  test "POST /login from a foreign Origin is rejected even with the token" do
    with_forgery_protection do
      fields = sign_in_form(@operator.email)
      post login_url, params: fields, headers: { "Origin" => "https://evil.example" }
      assert_response :unprocessable_entity
      assert_nil session[:user_id]
    end
  end

  test "logout requires the authenticity token" do
    with_forgery_protection do
      post login_url, params: sign_in_form(@operator.email)
      assert_equal @operator.id, session[:user_id]

      delete logout_url
      assert_response :unprocessable_entity
      assert_equal @operator.id, session[:user_id]
    end
  end

  test "a login form built like the previous release's posts successfully" do
    with_forgery_protection do
      get login_path
      html = controller.render_to_string(inline: ORIGIN_MAIN_LOGIN_FORM)
      form = Nokogiri::HTML(html).at_css("form") || flunk("no form")
      assert_equal "/login", form["action"]
      assert_equal "post", form["method"]
      fields = form.css("input[name]").to_h { |input| [ input["name"], input["value"].to_s ] }
      assert_equal %w[authenticity_token commit email password], fields.keys.sort
      # This app's own form uses the same field names.
      assert_equal fields.keys.sort, form_fields("form[action='/login']").keys.sort

      post form["action"], params: fields.merge("email" => @operator.email, "password" => "password123456")
      assert_redirected_to console_principals_path
      assert_equal @operator.id, session[:user_id]
    end
  end

  test "the pending page's sign-out form posts DELETE /logout like button_to" do
    with_forgery_protection do
      post login_url, params: sign_in_form(users(:pending_user).email)
      get pending_url
      fields = form_fields("form[action='/logout']")
      assert_equal %w[_method authenticity_token], fields.keys.sort
      assert_equal "delete", fields["_method"]
      assert_select "form[action='/logout'][method=post] button[type=submit]", text: "Sign out"

      post logout_url, params: fields
      assert_redirected_to login_path
      assert_nil session[:user_id]
    end
  end

  test "MCP authorize -> login -> POST /login returns to authorize with the query intact" do
    with_env("CENTAUR_MCP_PUBLIC_URL" => "http://localhost:3000/mcp",
             "CENTAUR_CONSOLE_PUBLIC_URL" => "http://www.example.com") do
      client = McpOauthClient.create!(
        name: "Amp", redirect_uris: [ "http://127.0.0.1:49152/callback" ],
        grant_types: McpOauthClient::DEFAULT_GRANT_TYPES,
        response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES, scopes: McpOauthClient::DEFAULT_SCOPES
      )
      params = {
        response_type: "code", client_id: client.public_client_id,
        redirect_uri: "http://127.0.0.1:49152/callback", scope: "mcp:tools", state: "st ate+/=",
        resource: "http://localhost:3000/mcp", code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
        code_challenge_method: "S256"
      }
      authorize_path = "/mcp/oauth/authorize?#{params.to_query}"

      with_forgery_protection do
        get authorize_path
        assert_redirected_to login_path
        follow_redirect!
        assert_response :ok

        post login_url, params: form_fields("form[action='/login']")
          .merge("email" => @operator.email, "password" => "password123456")
        assert_redirected_to authorize_path
        follow_redirect!
        assert_response :ok
        assert_select "form[action='/mcp/oauth/authorize']"
        consent = form_fields("form[action='/mcp/oauth/authorize']")
        assert_equal params.transform_keys(&:to_s), consent.except("authenticity_token")

        post "/mcp/oauth/authorize", params: consent.merge("decision" => "approve")
        redirect = URI.parse(response.location)
        query = Rack::Utils.parse_query(redirect.query)
        assert_equal "127.0.0.1", redirect.host
        assert_equal "st ate+/=", query["state"]
        assert query["code"].present?
      end
    end
  end

  test "logout clears the session" do
    post login_url, params: { email: @operator.email, password: "password123456" }
    delete logout_url
    assert_redirected_to login_path
    assert_equal "Signed out.", flash[:notice]
    assert_nil session[:user_id]
  end

  test "logout while signed out goes to the login page" do
    delete logout_url
    assert_redirected_to login_path
  end

  test "a pending user can sign out" do
    post login_url, params: { email: users(:pending_user).email, password: "password123456" }
    delete logout_url
    assert_redirected_to login_path
    assert_nil session[:user_id]
  end

  test "the pending page is reachable by a pending user" do
    post login_url, params: { email: users(:pending_user).email, password: "password123456" }
    get pending_url
    assert_response :ok
    assert_select "title", "Awaiting Approval · Centaur Console"
    assert_select "h1", "Awaiting approval"
    assert_select "strong", users(:pending_user).email
    assert_select "form[action=?]", logout_path do
      assert_select "input[name=_method][value=delete]", count: 1
    end
    assert_select "script", count: 0
  end

  test "an active user visiting the pending page is sent to the console" do
    post login_url, params: { email: @operator.email, password: "password123456" }
    get pending_url
    assert_redirected_to console_principals_path

    post login_url, params: { email: users(:member_user).email, password: "password123456" }
    get pending_url
    assert_redirected_to console_integrations_path
  end

  test "a signed-out visitor to the pending page is sent to login" do
    get pending_url
    assert_redirected_to login_path
  end
end
