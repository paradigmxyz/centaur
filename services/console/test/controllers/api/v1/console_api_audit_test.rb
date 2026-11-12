require "test_helper"

module Api
  module V1
    # Every non-GET /api/v1 request handled by Api::BaseController leaves one
    # structured console_api_audit line: actor, auth method (and API key oid,
    # never the token), controller/action, target ids, and status. Request
    # bodies and secret values never appear.
    class ConsoleApiAuditTest < ActionDispatch::IntegrationTest
      MARKER = "SYNTHETIC_AUDIT_BODY_MARKER".freeze

      setup do
        @io = StringIO.new
        @capture = ActiveSupport::Logger.new(@io)
        @capture.formatter = JsonLogFormatter.new
        @capture.level = :debug
        Rails.logger.broadcast_to(@capture)
      end

      teardown do
        Rails.logger.stop_broadcasting_to(@capture)
      end

      test "a session write is audited with the signed-in user" do
        admin = users(:acme_admin)
        sign_in_console(admin)
        role = roles(:acme_infra)
        token = console_csrf_token
        @io.truncate(0)
        @io.rewind

        patch api_v1_role_path(role.oid), params: { data: { name: role.name, description: MARKER } },
                                          headers: csrf_headers(token), as: :json
        assert_response :ok

        entry = sole_audit_entry
        assert_equal "console_api_audit", entry["event"]
        assert_equal admin.oid, entry["user_id"]
        assert_equal admin.email, entry["user_email"]
        assert_equal "session", entry["auth_method"]
        assert_nil entry["api_key_id"]
        assert_equal true, entry["authenticated"]
        assert_equal "api/v1/roles", entry["controller"]
        assert_equal "update", entry["action"]
        assert_equal "PATCH", entry["http_method"]
        assert_equal({ "id" => role.oid }, entry["target"])
        assert_equal 200, entry["status"]
        assert_not_includes @io.string.lines.grep(/console_api_audit/).join, MARKER
      end

      test "a bearer write is audited with the API key oid but never the token or secret value" do
        key = api_keys(:acme_ci_key)
        secret = static_secrets(:acme_prod_api_key)

        put api_v1_static_secret_path(secret.oid), params: {
          data: { name: secret.name, source: { type: "control_plane", secret: MARKER } }
        }, headers: bearer_headers, as: :json

        entry = sole_audit_entry
        assert_equal "api_key", entry["auth_method"]
        assert_equal key.oid, entry["api_key_id"]
        assert_equal key.user.oid, entry["user_id"]
        assert_equal key.user.email, entry["user_email"]
        assert_equal "api/v1/static_secrets", entry["controller"]
        assert_equal "update", entry["action"]
        assert_equal "PUT", entry["http_method"]
        assert_equal({ "id" => secret.oid }, entry["target"])
        assert_equal response.status, entry["status"]

        log = @io.string
        assert_not_includes log, ConsoleApiAuthHelper::ADMIN_TOKEN
        assert_not_includes log.lines.grep(/console_api_audit/).join, MARKER
      end

      test "a refused write is audited with the presented credential" do
        key = api_keys(:acme_ci_key)
        member = users(:member_user)

        post promote_api_v1_user_path(member.oid), headers: bearer_headers
        assert_response :forbidden
        assert_not member.reload.admin?

        entry = sole_audit_entry
        assert_equal "api_key", entry["auth_method"]
        assert_equal key.oid, entry["api_key_id"]
        assert_equal key.user.email, entry["user_email"]
        assert_equal false, entry["authenticated"]
        assert_equal "api/v1/users", entry["controller"]
        assert_equal "promote", entry["action"]
        assert_equal({ "id" => member.oid }, entry["target"])
        assert_equal 403, entry["status"]
      end

      test "a refused session write to a bearer-only endpoint is audited" do
        admin = users(:acme_admin)
        sign_in_console(admin)
        token = console_csrf_token
        @io.truncate(0)
        @io.rewind

        post api_v1_api_keys_path, params: { data: { name: MARKER } }, headers: csrf_headers(token), as: :json
        assert_response :forbidden

        entry = sole_audit_entry
        assert_equal "session", entry["auth_method"]
        assert_equal admin.oid, entry["user_id"]
        assert_equal false, entry["authenticated"]
        assert_equal "create", entry["action"]
        assert_equal 403, entry["status"]
        assert_not_includes entry.to_json, MARKER
      end

      test "an anonymous write is audited without an identity" do
        delete api_v1_principal_path(principals(:acme_user_bob).oid)
        assert_response :unauthorized

        entry = sole_audit_entry
        assert_nil entry["user_id"]
        assert_nil entry["auth_method"]
        assert_equal false, entry["authenticated"]
        assert_equal 401, entry["status"]
      end

      test "a demotion's audit line counts the API keys it revoked" do
        sign_in_console(users(:acme_admin))
        token = console_csrf_token
        target = users(:globex_admin)
        @io.truncate(0)
        @io.rewind

        post demote_api_v1_user_path(target.oid), headers: csrf_headers(token), as: :json
        assert_response :ok

        entry = sole_audit_entry
        assert_equal "demote", entry["action"]
        assert_equal({ "id" => target.oid }, entry["target"])
        assert_equal 1, entry["revoked_api_keys"]
        assert_not_includes @io.string, "iak_globex-ci-token"
      end

      test "reads are not audited" do
        get api_v1_principals_path, headers: bearer_headers
        assert_response :ok
        assert_empty audit_entries
      end

      private

      def audit_entries
        @io.string.lines.filter_map do |line|
          entry = JSON.parse(line)
          entry if entry["event"] == "console_api_audit"
        rescue JSON::ParserError
          nil
        end
      end

      def sole_audit_entry
        entries = audit_entries
        assert_equal 1, entries.size, @io.string
        entries.first
      end
    end
  end
end
