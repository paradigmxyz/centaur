require "test_helper"

# The SPA login and pending pages and the console-wide session gates around
# them. Password sign-in and sign-out (POST/DELETE /api/v1/session) and the
# login bootstrap (GET /api/v1/auth/config) are covered by
# test/controllers/api/v1/console_auth_pages_test.rb, which also ports the
# former POST /login and DELETE /logout form-endpoint tests.
class ConsoleLoginPagesTest < ActionDispatch::IntegrationTest
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

  test "the legacy form endpoints are gone" do
    post "/login", params: { email: @operator.email, password: ConsoleApiAuthHelper::TEST_PASSWORD }
    assert_response :not_found
    assert_nil session[:user_id]

    sign_in_console(@operator)
    delete "/logout"
    assert_response :not_found
    assert_equal @operator.id, session[:user_id]
  end

  test "unsafe methods do not replace the remembered login destination" do
    get "/console/credentials?kind=oauth", headers: HTML
    assert_equal "/console/credentials?kind=oauth", session[:return_to]

    post "/console/secrets/static", params: { static_secret: { foreign_id: "new-secret" } }
    assert_equal "/console/credentials?kind=oauth", session[:return_to]
  end

  test "a disabled user is signed out on their next console API request" do
    user = create_console_user
    sign_in_console(user)
    user.update!(status: :disabled)
    get api_v1_me_path
    assert_response :unauthorized
    assert_equal "account_disabled", json_body.dig("error", "details", "reason")
    assert_nil session[:user_id]
  end

  test "the pending page's session probe works for a pending user" do
    sign_in_console(users(:pending_user))
    get api_v1_me_path
    assert_response :ok
    assert_equal "pending", json_body.dig("data", "user", "status")
  end

  test "an active user's session probe reports the active status for the pending page redirect" do
    sign_in_console(@operator)
    get api_v1_me_path
    assert_equal "active", json_body.dig("data", "user", "status")
    assert_equal true, json_body.dig("data", "acting_admin")
  end
end
