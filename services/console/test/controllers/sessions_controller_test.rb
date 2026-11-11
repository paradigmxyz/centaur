require "test_helper"

# The redirect-only form endpoints POST /login and DELETE /logout, and the
# console-wide login/pending gates. The SPA login and pending pages (and their
# JSON endpoints) are covered by test/controllers/api/v1/console_auth_pages_test.rb.
class SessionsControllerTest < ActionDispatch::IntegrationTest
  HTML = { "Accept" => "text/html,application/xhtml+xml" }.freeze

  setup { @operator = users(:acme_admin) }

  test "the login and pending pages are served by the SPA" do
    Dir.mktmpdir do |dir|
      index = File.join(dir, "index.html")
      File.write(index, "<div id=\"root\"></div>")
      previous = SpaController.index_path
      SpaController.index_path = index
      [ "/login", "/login?return_to=%2Fconsole", "/pending" ].each do |path|
        get path, headers: HTML
        assert_response :ok, path
        assert_includes response.body, "<div id=\"root\">", path
      end
    ensure
      SpaController.index_path = previous
    end
  end

  test "valid credentials sign in and redirect to the console" do
    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_redirected_to console_principals_path
    assert_equal @operator.id, session[:user_id]
    assert_equal "Signed in as #{@operator.email}.", flash[:notice]
  end

  test "login redirects to the protected console URL the user first requested" do
    # A signed-out SPA deep link remembers itself (SpaController).
    get "/console/credentials?kind=oauth", headers: { "Accept" => "text/html" }
    assert_equal "/console/credentials?kind=oauth", session[:return_to]

    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_redirected_to "/console/credentials?kind=oauth"
    assert_equal @operator.id, session[:user_id]
    assert_nil session[:return_to]
  end

  test "unsafe methods do not replace the remembered login destination" do
    get "/console/credentials?kind=oauth", headers: { "Accept" => "text/html" }
    assert_equal "/console/credentials?kind=oauth", session[:return_to]

    post "/console/secrets/static", params: { static_secret: { foreign_id: "new-secret" } }
    assert_equal "/console/credentials?kind=oauth", session[:return_to]
  end

  test "password login rejects credentials when disabled" do
    ENV["CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED"] = "false"
    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_redirected_to login_path
    assert_nil session[:user_id]
    assert_equal "Email and password sign in is disabled.", flash[:alert]
  ensure
    ENV.delete("CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED")
  end

  test "a non-admin lands on integrations after login" do
    member = users(:member_user)
    post login_url, params: { email: member.email, password: "password123456" }
    assert_redirected_to console_integrations_path
    assert_equal member.id, session[:user_id]
  end

  test "email match is case-insensitive" do
    post login_url, params: { email: @operator.email.upcase, password: "password123456" }
    assert_equal @operator.id, session[:user_id]
  end

  test "invalid password returns to the login page without a session" do
    post login_url, params: { email: @operator.email, password: "wrong" }
    assert_redirected_to login_path
    assert_nil session[:user_id]
    assert_equal "Invalid email or password.", flash[:alert]
  end

  test "logout clears the session" do
    post login_url, params: { email: @operator.email, password: "password123456" }
    delete logout_url
    assert_redirected_to login_path
    assert_nil session[:user_id]
    assert_equal "Signed out.", flash[:notice]
  end

  test "a pending user is signed in but routed to the holding page" do
    pending = users(:pending_user)
    post login_url, params: { email: pending.email, password: "password123456" }
    assert_redirected_to pending_path
    assert_equal pending.id, session[:user_id]
    assert_equal "Your account is awaiting approval.", flash[:notice]
  end

  test "a disabled user cannot sign in" do
    disabled = users(:disabled_user)
    post login_url, params: { email: disabled.email, password: "password123456" }
    assert_redirected_to login_path
    assert_nil session[:user_id]
    assert_equal "Your account is disabled.", flash[:alert]
  end


  test "a disabled user hitting a console page is signed out" do
    user = create_console_user
    sign_in_console(user)
    user.update!(status: :disabled)
    get console_roles_url
    assert_redirected_to login_path
    assert_equal "Your account is disabled.", flash[:alert]
    assert_nil session[:user_id]
  end

  test "the pending page's session probe works for a pending user" do
    post login_url, params: { email: users(:pending_user).email, password: "password123456" }
    get api_v1_me_path
    assert_response :ok
    assert_equal "pending", json_body.dig("data", "user", "status")
  end

  test "an active user's session probe reports the active status for the pending page redirect" do
    post login_url, params: { email: @operator.email, password: "password123456" }
    get api_v1_me_path
    assert_equal "active", json_body.dig("data", "user", "status")
    assert_equal true, json_body.dig("data", "acting_admin")
  end
end
