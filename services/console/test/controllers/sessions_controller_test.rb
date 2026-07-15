require "test_helper"

class SessionsControllerTest < ActionDispatch::IntegrationTest
  setup { @operator = users(:acme_admin) }

  test "GET new renders the login form" do
    get login_url
    assert_response :ok
    assert_select "form[action=?]", login_path
  end

  test "GET new hides the password form when password login is disabled" do
    ENV["CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED"] = "false"
    get login_url
    assert_response :ok
    assert_select "form[action=?]", login_path, count: 0
    assert_select "input[name=?]", "email", count: 0
  ensure
    ENV.delete("CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED")
  end

  test "valid credentials sign in and redirect to the console" do
    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_redirected_to console_principals_path
    assert_equal @operator.id, session[:user_id]
  end

  test "login redirects to the protected console URL the user first requested" do
    get console_credentials_url(kind: "oauth")
    assert_redirected_to login_path
    assert_equal "/console/credentials?kind=oauth", session[:return_to]

    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_redirected_to "/console/credentials?kind=oauth"
    assert_equal @operator.id, session[:user_id]
    assert_nil session[:return_to]
  end

  test "unsafe methods do not replace the remembered login destination" do
    get console_credentials_url(kind: "oauth")
    assert_equal "/console/credentials?kind=oauth", session[:return_to]

    post console_roles_url, params: { role: { foreign_id: "new-role", namespace: "default" } }
    assert_redirected_to login_path
    assert_equal "/console/credentials?kind=oauth", session[:return_to]
  end

  test "password login rejects credentials when disabled" do
    ENV["CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED"] = "false"
    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_response :not_found
    assert_nil session[:user_id]
    assert_select "div", /Email and password sign in is disabled/
  ensure
    ENV.delete("CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED")
  end

  test "a non-admin lands on the threads view after login" do
    member = users(:member_user)
    post login_url, params: { email: member.email, password: "password123456" }
    assert_redirected_to console_threads_path
    assert_equal member.id, session[:user_id]
  end

  test "email match is case-insensitive" do
    post login_url, params: { email: @operator.email.upcase, password: "password123456" }
    assert_equal @operator.id, session[:user_id]
  end

  test "invalid password re-renders the form without a session" do
    post login_url, params: { email: @operator.email, password: "wrong" }
    assert_response :unprocessable_entity
    assert_nil session[:user_id]
    assert_select "div", /Invalid email or password/
  end

  test "logout clears the session" do
    post login_url, params: { email: @operator.email, password: "password123456" }
    delete logout_url
    assert_redirected_to login_path
    assert_nil session[:user_id]
  end

  test "a pending user is signed in but routed to the holding page" do
    pending = users(:pending_user)
    post login_url, params: { email: pending.email, password: "password123456" }
    assert_redirected_to pending_path
    assert_equal pending.id, session[:user_id]
  end

  test "a disabled user cannot sign in" do
    disabled = users(:disabled_user)
    post login_url, params: { email: disabled.email, password: "password123456" }
    assert_response :unprocessable_entity
    assert_nil session[:user_id]
  end

  test "a pending user hitting a console page is bounced to the holding page" do
    post login_url, params: { email: users(:pending_user).email, password: "password123456" }
    get console_principals_url
    assert_redirected_to pending_path
  end

  test "the pending page is reachable by a pending user" do
    post login_url, params: { email: users(:pending_user).email, password: "password123456" }
    get pending_url
    assert_response :ok
  end

  test "an active user visiting the pending page is sent to the console" do
    post login_url, params: { email: @operator.email, password: "password123456" }
    get pending_url
    assert_redirected_to console_principals_path
  end
end
