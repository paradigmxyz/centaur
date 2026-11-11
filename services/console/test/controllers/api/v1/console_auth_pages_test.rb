require "test_helper"

module Api
  module V1
    # GET /api/v1/auth/config (public login-page bootstrap) and
    # POST/DELETE /api/v1/session (SPA password sign-in and sign-out). Ports
    # the legacy sessions_controller_test login-page assertions.
    class ConsoleAuthPagesTest < ActionDispatch::IntegrationTest
      setup { @operator = users(:acme_admin) }

      teardown do
        ENV.delete("CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED")
        %w[GOOGLE SLACK].each do |p|
          ENV.delete("CENTAUR_CONSOLE_#{p}_CLIENT_ID")
          ENV.delete("CENTAUR_CONSOLE_#{p}_CLIENT_SECRET")
        end
      end

      # The CSRF token a signed-out browser gets from the login bootstrap.
      def login_csrf_token
        get api_v1_auth_config_path
        json_body.dig("data", "csrf_token")
      end

      def api_sign_in(email, password = "password123456", token: login_csrf_token, headers: {})
        post api_v1_session_path, params: { data: { email: email, password: password } },
             headers: { "X-CSRF-Token" => token }.compact.merge(headers), as: :json
      end

      # --- GET /api/v1/auth/config --------------------------------------------

      test "auth config is public and describes the sign-in methods" do
        get api_v1_auth_config_path
        assert_response :ok
        assert_equal "no-store", response.headers["Cache-Control"]
        data = json_body["data"]
        assert_equal true, data["password_login"]
        assert_equal [], data["providers"]
        assert data["csrf_token"].present?
        assert_equal false, data["signed_in"]
        assert_nil data["redirect_to"]
        assert_equal({}, data["flash"])
        assert_nil data["oauth_result"]
      end

      test "auth config lists a button for each configured provider only" do
        ENV["CENTAUR_CONSOLE_GOOGLE_CLIENT_ID"] = "google-login-client-id"
        ENV["CENTAUR_CONSOLE_GOOGLE_CLIENT_SECRET"] = "google-login-secret"
        # Slack without a secret is not configured, so it must not appear.
        ENV["CENTAUR_CONSOLE_SLACK_CLIENT_ID"] = "slack-login-client-id"

        get api_v1_auth_config_path
        assert_equal [ { "key" => "google", "label" => "Google", "start_path" => auth_start_path(provider: "google") } ],
                     json_body.dig("data", "providers")
      end

      test "auth config reports password login disabled" do
        ENV["CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED"] = "false"
        get api_v1_auth_config_path
        assert_equal false, json_body.dig("data", "password_login")
      end

      test "auth config ignores a bearer key and never exposes credentials" do
        get api_v1_auth_config_path, headers: bearer_headers
        assert_response :ok
        assert_equal false, json_body.dig("data", "signed_in")
        assert_not_includes response.body, ConsoleApiAuthHelper::ADMIN_TOKEN
      end

      test "auth config sends an already signed-in active user to the console" do
        sign_in_console(@operator)
        get api_v1_auth_config_path
        assert_equal true, json_body.dig("data", "signed_in")
        assert_equal console_principals_path, json_body.dig("data", "redirect_to")

        get api_v1_auth_config_path, params: { return_to: "/console/credentials?kind=oauth" }
        assert_equal "/console/credentials?kind=oauth", json_body.dig("data", "redirect_to")

        sign_in_console(users(:member_user))
        get api_v1_auth_config_path
        assert_equal console_integrations_path, json_body.dig("data", "redirect_to")
      end

      test "auth config never sends a signed-in user off-origin" do
        sign_in_console(@operator)
        [ "https://evil.example/", "//evil.example", "/\\evil.example", "/console/../api/v1/me",
          "/api/v1/principals", "javascript:alert(1)" ].each do |target|
          get api_v1_auth_config_path, params: { return_to: target }
          assert_equal console_principals_path, json_body.dig("data", "redirect_to"), target
        end
      end

      test "auth config shows the login page to pending users and signs out disabled ones" do
        sign_in_console(users(:pending_user))
        get api_v1_auth_config_path
        assert_equal true, json_body.dig("data", "signed_in")
        assert_nil json_body.dig("data", "redirect_to")

        user = create_console_user
        sign_in_console(user)
        user.update!(status: :disabled)
        get api_v1_auth_config_path
        assert_equal false, json_body.dig("data", "signed_in")
        assert_nil session[:user_id]
      end

      test "auth config hands off the flash exactly once" do
        post login_path, params: { email: @operator.email, password: "wrong" }
        assert_redirected_to login_path

        get api_v1_auth_config_path
        assert_equal({ "alert" => "Invalid email or password." }, json_body.dig("data", "flash"))
        get api_v1_auth_config_path
        assert_equal({}, json_body.dig("data", "flash"))
      end

      test "auth config remembers an allowlisted return_to for the next sign-in" do
        get api_v1_auth_config_path, params: { return_to: "/console/credentials?kind=oauth" }
        api_sign_in(@operator.email)
        assert_response :ok
        assert_equal "/console/credentials?kind=oauth", json_body.dig("data", "redirect_to")
        assert_nil session[:return_to]
      end

      test "auth config ignores return_to values outside the allowlist" do
        [ "https://evil.example/", "//evil.example/x", "/\\evil.example", "/logout", "/api/v1/me",
          "/console/%2e%2e/api", "/console/../oauth/x/callback" ].each do |target|
          reset!
          get api_v1_auth_config_path, params: { return_to: target }
          assert_nil session[:return_to], target
          api_sign_in(@operator.email)
          assert_equal console_principals_path, json_body.dig("data", "redirect_to"), target
        end
      end

      # --- POST /api/v1/session -------------------------------------------------

      test "valid credentials sign in and return the console landing page" do
        api_sign_in(@operator.email)
        assert_response :ok
        assert_equal({ "status" => "active", "redirect_to" => console_principals_path }, json_body["data"])
        assert_equal @operator.id, session[:user_id]

        get api_v1_me_path
        assert_equal "Signed in as #{@operator.email}.", json_body.dig("data", "flash", "notice")
      end

      test "login returns to the protected console URL the user first requested" do
        get console_credentials_url(kind: "oauth")
        assert_redirected_to login_path
        assert_equal "/console/credentials?kind=oauth", session[:return_to]

        api_sign_in(@operator.email)
        assert_equal "/console/credentials?kind=oauth", json_body.dig("data", "redirect_to")
        assert_nil session[:return_to]
      end

      test "login resets the session (no fixation) but keeps the return path" do
        get console_credentials_url(kind: "oauth")
        old_session_id = session.id.to_s
        api_sign_in(@operator.email)
        assert_not_equal old_session_id, session.id.to_s
        assert_equal "/console/credentials?kind=oauth", json_body.dig("data", "redirect_to")
      end

      test "a non-admin lands on integrations after login" do
        api_sign_in(users(:member_user).email)
        assert_equal console_integrations_path, json_body.dig("data", "redirect_to")
      end

      test "email match is case-insensitive and trims whitespace" do
        api_sign_in("  #{@operator.email.upcase} ")
        assert_response :ok
        assert_equal @operator.id, session[:user_id]
      end

      test "invalid password is rejected without a session" do
        api_sign_in(@operator.email, "wrong")
        assert_response :unprocessable_entity
        assert_equal "Invalid email or password.", json_body.dig("error", "message")
        assert_equal "invalid_credentials", json_body.dig("error", "details", "reason")
        assert_nil session[:user_id]
      end

      test "an unknown email is rejected like a bad password" do
        api_sign_in("nobody@example.test")
        assert_response :unprocessable_entity
        assert_equal "Invalid email or password.", json_body.dig("error", "message")
      end

      test "password login rejects credentials when disabled" do
        token = login_csrf_token
        ENV["CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED"] = "false"
        api_sign_in(@operator.email, token: token)
        assert_response :not_found
        assert_equal "Email and password sign in is disabled.", json_body.dig("error", "message")
        assert_nil session[:user_id]
      end

      test "a pending user is signed in but routed to the holding page" do
        pending = users(:pending_user)
        api_sign_in(pending.email)
        assert_response :ok
        assert_equal({ "status" => "pending", "redirect_to" => "/pending" }, json_body["data"])
        assert_equal pending.id, session[:user_id]

        get api_v1_me_path
        assert_equal "pending", json_body.dig("data", "user", "status")
        assert_equal "Your account is awaiting approval.", json_body.dig("data", "flash", "notice")
      end

      test "a disabled user cannot sign in" do
        api_sign_in(users(:disabled_user).email)
        assert_response :unprocessable_entity
        assert_equal "Your account is disabled.", json_body.dig("error", "message")
        assert_nil session[:user_id]
      end

      test "sign-in requires the CSRF token (login CSRF)" do
        login_csrf_token
        api_sign_in(@operator.email, token: nil)
        assert_response :forbidden
        assert_equal "csrf", json_body.dig("error", "details", "reason")
        assert_nil session[:user_id]

        api_sign_in(@operator.email, token: "not-a-token")
        assert_response :forbidden
        assert_nil session[:user_id]
      end

      test "sign-in from a foreign Origin is rejected even with the token" do
        api_sign_in(@operator.email, headers: { "Origin" => "https://evil.example" })
        assert_response :forbidden
        assert_nil session[:user_id]
      end

      test "a CSRF token from before sign-in cannot be replayed after it" do
        token = login_csrf_token
        api_sign_in(@operator.email, token: token)
        assert_response :ok

        delete api_v1_session_path, headers: { "X-CSRF-Token" => token }
        assert_response :forbidden
        assert_equal @operator.id, session[:user_id]
      end

      test "a pending user hitting a console page is bounced to the holding page" do
        api_sign_in(users(:pending_user).email)
        get console_credentials_url
        assert_redirected_to pending_path
        get api_v1_principals_path
        assert_response :forbidden
        assert_equal "account_pending", json_body.dig("error", "details", "reason")
      end

      # --- DELETE /api/v1/session -----------------------------------------------

      test "logout clears the session and leaves a notice for the login page" do
        sign_in_console(@operator)
        delete api_v1_session_path, headers: csrf_headers
        assert_response :no_content
        assert_nil session[:user_id]

        get api_v1_auth_config_path
        assert_equal({ "notice" => "Signed out." }, json_body.dig("data", "flash"))
      end

      test "a pending user can sign out" do
        sign_in_console(users(:pending_user))
        delete api_v1_session_path, headers: csrf_headers
        assert_response :no_content
        assert_nil session[:user_id]
      end

      test "logout requires the CSRF token" do
        sign_in_console(@operator)
        delete api_v1_session_path
        assert_response :forbidden
        assert_equal @operator.id, session[:user_id]
      end
    end
  end
end
