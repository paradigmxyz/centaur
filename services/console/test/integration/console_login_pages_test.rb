require "test_helper"

# The console-wide session gates around the server-rendered login and pending
# pages (sessions_controller_test covers the pages and their form endpoints),
# and the SPA's sign-out endpoint, DELETE /api/v1/console/session.
class ConsoleLoginPagesTest < ActionDispatch::IntegrationTest
  HTML = { "Accept" => "text/html,application/xhtml+xml" }.freeze

  setup { @operator = users(:acme_admin) }

  test "unsafe methods do not replace the remembered login destination" do
    get "/console/credentials?kind=oauth", headers: HTML
    assert_equal "/console/credentials?kind=oauth", session[:return_to]

    post "/console/secrets/static", params: { static_secret: { foreign_id: "new-secret" } }
    assert_equal "/console/credentials?kind=oauth", session[:return_to]
  end

  test "a disabled user's next login page says the account is disabled" do
    user = create_console_user
    sign_in_console(user)
    user.update!(status: :disabled)
    get api_v1_console_me_path
    assert_response :unauthorized

    get login_path(return_to: "/console/skills")
    assert_response :ok
    assert_select "[role=alert]", text: "Your account is disabled."

    get login_path
    assert_select "[role=alert]", count: 0
  end

  test "SPA sign-out clears the session and leaves a notice for the login page" do
    sign_in_console(@operator)
    console_sign_out
    assert_response :no_content
    assert_nil session[:user_id]

    get login_path
    assert_select "[role=status]", text: "Signed out."
  end

  test "a pending user can sign out from the SPA endpoint" do
    sign_in_console(users(:pending_user))
    delete api_v1_console_session_path, headers: csrf_headers
    assert_response :no_content
    assert_nil session[:user_id]
  end

  test "SPA sign-out requires the CSRF token" do
    sign_in_console(@operator)
    delete api_v1_console_session_path
    assert_response :forbidden
    assert_equal @operator.id, session[:user_id]
  end

  test "a CSRF token from before sign-in cannot be replayed after it" do
    get login_path
    token = controller.send(:form_authenticity_token)
    post login_path, params: { email: @operator.email, password: ConsoleApiAuthHelper::TEST_PASSWORD }
    assert_equal @operator.id, session[:user_id]

    delete api_v1_console_session_path, headers: { "X-CSRF-Token" => token }
    assert_response :forbidden
    assert_equal @operator.id, session[:user_id]
  end
end
