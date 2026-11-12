require "test_helper"

module Api
  module V1
    # Requests use the console session cookie (+ CSRF) as the SPA does.
    class ConsoleUsersTest < ActionDispatch::IntegrationTest
      setup do
        @admin = users(:acme_admin)
        sign_in_console(@admin)
      end

      def post_action(action, user, headers: csrf_headers)
        post public_send(:"#{action}_api_v1_user_path", user.oid), headers: headers, as: :json
      end

      test "user endpoints are admin-only for every caller type" do
        target = users(:pending_user)
        assert_admin_only_api :get, api_v1_users_path
        assert_admin_only_api :post, approve_api_v1_user_path(target.oid)
        assert_admin_only_api :post, disable_api_v1_user_path(users(:member_user).oid)
        assert_admin_only_api :post, promote_api_v1_user_path(target.oid)
        assert_admin_only_api :post, demote_api_v1_user_path(users(:globex_admin).oid)

        assert target.reload.pending?
        assert_not target.admin?
        assert users(:member_user).reload.active?
        assert users(:globex_admin).reload.admin?
      end

      test "a signed-out request is refused" do
        reset!
        get api_v1_users_path
        assert_response :unauthorized
      end

      test "a non-admin cannot list users or perform actions" do
        sign_in_console(users(:member_user))
        get api_v1_users_path
        assert_response :forbidden

        target = users(:pending_user)
        post_action(:approve, target)
        assert_response :forbidden
        assert target.reload.pending?

        admin = users(:globex_admin)
        post_action(:demote, admin)
        assert_response :forbidden
        assert admin.reload.admin?
      end

      test "the index lists every user with status, role, and the pending ones" do
        get api_v1_users_path
        assert_response :ok
        rows = json_body["data"].index_by { |user| user["email"] }

        assert_equal User.count, json_body.dig("meta", "total")
        assert_equal "pending", rows.dig("pending@acme.example", "status")
        assert_equal true, rows.dig("admin@acme.example", "admin")
        assert_equal true, rows.dig("admin@acme.example", "current")
        assert_equal false, rows.dig("admin@globex.example", "current")
        assert_equal users(:member_user).oid, rows.dig("member@acme.example", "id")
        # admins first, then by email
        emails = json_body["data"].map { |user| user["email"] }
        admins = json_body["data"].select { |user| user["admin"] }.map { |user| user["email"] }
        assert_equal admins.sort, emails.first(admins.size)
      end

      test "the index filters by status" do
        get api_v1_users_path(status: "pending")
        assert_equal [ "pending@acme.example" ], json_body["data"].map { |user| user["email"] }

        get api_v1_users_path(status: "bogus")
        assert_response :bad_request
      end

      test "the index reports linked identity providers" do
        get api_v1_users_path
        rows = json_body["data"].index_by { |user| user["email"] }
        assert_equal %w[google], rows.dig("admin@acme.example", "identity_providers")   # linked via Google
        assert_equal %w[slack], rows.dig("pending@acme.example", "identity_providers")  # linked via Slack
        assert_equal [], rows.dig("member@acme.example", "identity_providers")          # password only
      end

      test "the index flags the caller so the page can hide self-demotion" do
        get api_v1_users_path
        rows = json_body["data"].index_by { |user| user["email"] }
        assert_equal true, rows.dig(users(:acme_admin).email, "current")
        assert_equal false, rows.dig(users(:globex_admin).email, "current")
      end

      test "approve activates a pending user and records the approver" do
        pending = users(:pending_user)
        post_action(:approve, pending)
        assert_response :ok
        assert pending.reload.active?
        assert_equal @admin, pending.approved_by
        assert_equal "active", json_body.dig("data", "status")
        assert_equal({ "id" => @admin.oid, "email" => @admin.email }, json_body.dig("data", "approved_by"))
      end

      test "disable revokes an active user" do
        target = users(:member_user)
        post_action(:disable, target)
        assert_response :ok
        assert target.reload.disabled?
        assert_equal "disabled", json_body.dig("data", "status")
      end

      test "disable revokes outstanding MCP OAuth refresh tokens" do
        target = users(:member_user)
        refresh = McpOauthRefreshToken.create!(
          mcp_oauth_client: McpOauthClient.create!(
            name: "Amp",
            redirect_uris: [ "http://127.0.0.1:49152/callback" ],
            grant_types: McpOauthClient::DEFAULT_GRANT_TYPES,
            response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES,
            scopes: McpOauthClient::DEFAULT_SCOPES
          ),
          user: target,
          principal: principals(:acme_channel),
          resource: "http://localhost:3000/mcp",
          scopes: [ "mcp:tools" ]
        )

        post_action(:disable, target)

        assert_response :ok
        assert target.reload.disabled?
        assert refresh.reload.revoked_at.present?
      end

      test "a disabled user's console session is signed out on its next request" do
        target = create_console_user
        other = open_session
        other.extend(ConsoleApiAuthHelper)
        other.sign_in_console(target)

        post_action(:disable, target)
        assert_response :ok

        other.get api_v1_me_path
        assert_equal 401, other.response.status
      end

      test "an admin cannot disable their own account" do
        post_action(:disable, @admin)
        assert_response :unprocessable_entity
        assert_equal "You can't disable your own account.", json_body.dig("error", "message")
        assert @admin.reload.active?

        post_action(:disable, @admin, headers: bearer_headers)
        assert_response :forbidden
        assert @admin.reload.active?
      end

      test "promote makes a user an active admin" do
        target = users(:pending_user)
        post_action(:promote, target)
        assert_response :ok
        target.reload
        assert target.admin?
        assert target.active?
      end

      test "demote makes an admin a user" do
        target = users(:globex_admin)
        post_action(:demote, target)
        assert_response :ok
        assert_not target.reload.admin?
        assert_equal false, json_body.dig("data", "admin")
      end

      test "an admin cannot demote their own account" do
        post_action(:demote, @admin)
        assert_response :unprocessable_entity
        assert_equal "You can't demote your own account.", json_body.dig("error", "message")
        assert @admin.reload.admin?
      end

      test "an unknown user is a 404" do
        post approve_api_v1_user_path("usr_missing"), headers: csrf_headers, as: :json
        assert_response :not_found
      end
    end
  end
end
