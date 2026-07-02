require "test_helper"

class SessionsControllerTest < ActionDispatch::IntegrationTest
  setup { @operator = users(:acme_admin) }

  test "GET new renders the login form" do
    get login_url
    assert_response :ok
    assert_select "form[action=?]", login_path
  end

  test "valid credentials sign in and redirect to the console" do
    post login_url, params: { email: @operator.email, password: "password123456" }
    assert_redirected_to console_principals_path
    assert_equal @operator.id, session[:user_id]
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
