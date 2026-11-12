require "test_helper"

module Api
  module V1
    # The /api/v1 authentication matrix: bearer API keys and console session
    # cookies (with CSRF), account status, and admin-by-default authorization.
    class SessionAuthTest < ActionDispatch::IntegrationTest
      test "anonymous requests are rejected with the API key message" do
        get api_v1_principals_path
        assert_response :unauthorized
        assert_equal "invalid or missing API key", json_body.dig("error", "message")
      end

      test "an admin session cookie authenticates GET requests without a CSRF token" do
        sign_in_console(users(:acme_admin))
        get api_v1_principals_path
        assert_response :ok
        assert json_body["data"].any?
      end

      test "an admin bearer key still authenticates without a session" do
        get api_v1_principals_path, headers: bearer_headers
        assert_response :ok
      end

      test "a bad bearer token never falls back to the session cookie" do
        sign_in_console(users(:acme_admin))
        get api_v1_principals_path, headers: bearer_headers("iak_bogus")
        assert_response :unauthorized
      end

      test "a non-Bearer Authorization header never falls back to the session cookie" do
        sign_in_console(users(:acme_admin))
        get api_v1_principals_path, headers: { "Authorization" => "Basic Zm9vOmJhcg==" }
        assert_response :unauthorized
      end

      test "cookie-authenticated writes require the CSRF token" do
        sign_in_console(users(:acme_admin))

        assert_no_difference -> { Principal.count } do
          post api_v1_principals_path, params: { data: { foreign_id: "csrf-missing" } }, as: :json
        end
        assert_response :forbidden
        assert_equal "invalid or missing CSRF token", json_body.dig("error", "message")
        assert_equal "csrf", json_body.dig("error", "details", "reason")

        assert_no_difference -> { Principal.count } do
          post api_v1_principals_path, params: { data: { foreign_id: "csrf-bad" } },
               headers: csrf_headers("not-a-token"), as: :json
        end
        assert_response :forbidden

        assert_difference -> { Principal.count }, 1 do
          post api_v1_principals_path, params: { data: { foreign_id: "csrf-ok" } },
               headers: csrf_headers, as: :json
        end
        assert_response :created
        assert_equal users(:acme_admin), Principal.find_by!(foreign_id: "csrf-ok").created_by
      end

      test "cookie writes from a foreign Origin are rejected even with the token" do
        sign_in_console(users(:acme_admin))
        token = console_csrf_token
        post api_v1_principals_path, params: { data: { foreign_id: "csrf-origin" } },
             headers: csrf_headers(token).merge("Origin" => "https://evil.example"), as: :json
        assert_response :forbidden
      end

      test "bearer writes skip CSRF" do
        post api_v1_principals_path, params: { data: { foreign_id: "bearer-write" } },
             headers: bearer_headers, as: :json
        assert_response :created
      end

      test "admin-only endpoints refuse every non-admin combination" do
        assert_admin_only_api :get, api_v1_principals_path
        assert_admin_only_api :post, api_v1_principals_path, params: { data: { foreign_id: "matrix" } }
        assert_equal 0, Principal.where(foreign_id: "matrix").count
      end

      test "a non-admin bearer key keeps the existing error message" do
        get api_v1_principals_path, headers: bearer_headers(ConsoleApiAuthHelper::MEMBER_TOKEN)
        assert_response :forbidden
        assert_equal "API key owner is not an admin", json_body.dig("error", "message")
      end

      test "a disabled session is signed out" do
        user = create_console_user(admin: true)
        sign_in_console(user)
        user.update!(status: :disabled)

        get api_v1_me_path
        assert_response :unauthorized
        assert_equal "account_disabled", json_body.dig("error", "details", "reason")

        user.update!(status: :active)
        get api_v1_me_path
        assert_response :unauthorized
        assert_equal "console session required", json_body.dig("error", "message")
      end

      test "me describes an admin session" do
        sign_in_console(users(:acme_admin))
        get api_v1_me_path

        assert_response :ok
        assert_equal "no-store", response.headers["Cache-Control"]
        data = json_body["data"]
        assert_equal users(:acme_admin).oid, data.dig("user", "id")
        assert_equal "admin@acme.example", data.dig("user", "email")
        assert_equal true, data.dig("user", "admin")
        assert_equal "session", data["auth_method"]
        assert_equal true, data["acting_admin"]
        assert data["csrf_token"].present?
        assert_equal true, data.dig("nav", "control")
        assert_equal true, data.dig("nav", "workflows")
        assert_equal false, data.dig("nav", "api_clients")
        assert_equal true, data.dig("nav", "integrations")
      end

      test "me hands off a pending redirect flash exactly once" do
        sign_in_console(users(:acme_admin))
        get api_v1_me_path
        assert_equal({ "notice" => "Signed in as admin@acme.example." }, json_body.dig("data", "flash"))

        get api_v1_me_path
        assert_equal({}, json_body.dig("data", "flash"))
      end

      test "a redirect flash survives the SPA shell request" do
        sign_in_console(users(:acme_admin))
        get "/console/spa-only-page", headers: { "Accept" => "text/html" }
        get api_v1_me_path
        assert_equal "Signed in as admin@acme.example.", json_body.dig("data", "flash", "notice")
      end

      test "me shows the api clients section only when the feature is enabled" do
        with_env("CENTAUR_CONSOLE_API_CLIENTS_ENABLED" => "true") do
          sign_in_console(users(:acme_admin))
          get api_v1_me_path
          assert_equal true, json_body.dig("data", "nav", "api_clients")
          assert_equal true, json_body.dig("data", "features", "api_clients")
        end
      end

      test "me describes a regular operator session" do
        sign_in_console(users(:member_user))
        get api_v1_me_path

        assert_response :ok
        data = json_body["data"]
        assert_equal false, data["acting_admin"]
        assert_equal false, data.dig("nav", "control")
        assert_equal false, data.dig("nav", "workflows")
        assert_equal true, data.dig("nav", "integrations")
        assert_equal true, data.dig("nav", "skills")
        assert_equal true, data.dig("nav", "scheduled_tasks")
      end

      test "me is reachable by a pending account so the SPA can show the holding page" do
        sign_in_console(users(:pending_user))
        get api_v1_me_path

        assert_response :ok
        assert_equal "pending", json_body.dig("data", "user", "status")
        assert_equal false, json_body.dig("data", "nav", "integrations")

        get api_v1_principals_path
        assert_response :forbidden
        assert_equal "account_pending", json_body.dig("error", "details", "reason")
      end

      test "me is session-only" do
        get api_v1_me_path
        assert_response :unauthorized
        assert_equal "session_required", json_body.dig("error", "details", "reason")

        [ bearer_headers, bearer_headers(ConsoleApiAuthHelper::DISABLED_TOKEN) ].each do |headers|
          get api_v1_me_path, headers: headers
          assert_response :forbidden
          assert_equal "session_required", json_body.dig("error", "details", "reason")
          assert_nil json_body["data"]
        end
      end

      test "session destroy refuses bearer callers" do
        delete api_v1_session_path, headers: bearer_headers
        assert_response :forbidden
        assert_equal "session_required", json_body.dig("error", "details", "reason")
      end
    end
  end
end
