require "test_helper"

module Api
  module V1
    class RolesControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      test "rejects unauthenticated requests" do
        get api_v1_role_url(id: "role_unknown")
        assert_response :unauthorized
      end

      test "GET returns a role" do
        role = roles(:acme_infra)
        get api_v1_role_url(id: role.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal role.oid, data["id"]
        assert_equal "acme", data["namespace"]
        assert_equal "infra", data["foreign_id"]
        assert_equal "Infra", data["name"]
        assert_equal [], data["slack_channel_permissions"]
      end

      test "GET returns 404 for an unknown oid" do
        get api_v1_role_url(id: "role_nope"), headers: auth_headers
        assert_response :not_found
      end

      test "POST creates a role" do
        body = { data: { namespace: "acme", foreign_id: "payments", name: "Payments" } }
        assert_difference -> { Role.count } => 1 do
          post api_v1_roles_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_match(/\Arole_/, data["id"])
        assert_equal "payments", data["foreign_id"]
      end

      test "POST creates a role with Slack channel permissions" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "support",
            slack_channel_permissions: [
              {
                channel_id: " c0123456789 ",
                upload_enabled: true,
                history_enabled: true
              }
            ]
          }
        }

        post api_v1_roles_url, params: body.to_json, headers: auth_headers
        assert_response :created

        role = Role.find_by!(namespace: "acme", foreign_id: "support")
        assert_equal [ "C0123456789" ], role.slack_channel_permissions.pluck(:channel_id)
        assert_equal role.slack_channel_permissions_payload,
                     json_body.dig("data", "slack_channel_permissions")
      end

      test "POST defaults the namespace" do
        body = { data: { name: "No namespace" } }
        post api_v1_roles_url, params: body.to_json, headers: auth_headers
        assert_response :created
        assert_equal "default", json_body.dig("data", "namespace")
      end

      test "POST returns 422 when (namespace, foreign_id) already exists" do
        existing = roles(:acme_infra)
        body = { data: { namespace: existing.namespace, foreign_id: existing.foreign_id } }
        assert_no_difference -> { Role.count } do
          post api_v1_roles_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_content
      end

      test "POST returns 400 when data is missing" do
        post api_v1_roles_url, params: { namespace: "acme" }.to_json, headers: auth_headers
        assert_response :bad_request
      end

      test "PUT updates name and labels but not namespace" do
        role = roles(:acme_infra)
        body = { data: { name: "Infrastructure", namespace: "globex", labels: { "tier" => "base" } } }
        put api_v1_role_url(id: role.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        role.reload
        assert_equal "Infrastructure", role.name
        assert_equal({ "tier" => "base" }, role.labels)
        assert_equal "acme", role.namespace
      end

      test "PUT replaces role Slack channel permissions when present" do
        role = roles(:acme_infra)
        role.slack_channel_permissions.create!(
          channel_id: "C1111111111",
          upload_enabled: true
        )
        body = {
          data: {
            slack_channel_permissions: [
              {
                channel_id: "G9876543210",
                download_enabled: true
              }
            ]
          }
        }

        put api_v1_role_url(id: role.oid), params: body.to_json, headers: auth_headers
        assert_response :ok
        assert_equal [ "G9876543210" ], role.slack_channel_permissions.reload.pluck(:channel_id)
        assert_equal [ "G9876543210" ],
                     json_body.dig("data", "slack_channel_permissions").pluck("channel_id")
      end

      test "POST upserts one role Slack channel permission idempotently" do
        role = roles(:acme_infra)
        body = {
          data: {
            channel_id: " c0123456789 ",
            upload_enabled: true,
            download_enabled: false,
            history_enabled: false
          }
        }

        post "/api/v1/roles/#{role.oid}/slack_channel_permissions",
             params: body.to_json,
             headers: auth_headers
        assert_response :created

        body[:data][:upload_enabled] = false
        body[:data][:history_enabled] = true
        assert_no_difference -> { role.slack_channel_permissions.count } do
          post "/api/v1/roles/#{role.oid}/slack_channel_permissions",
               params: body.to_json,
               headers: auth_headers
        end
        assert_response :ok

        permission = role.slack_channel_permissions.reload.sole
        assert_not permission.upload_enabled
        assert_predicate permission, :history_enabled
      end

      test "POST leaves omitted flags unchanged on an existing role permission" do
        role = roles(:acme_infra)
        permission = role.slack_channel_permissions.create!(
          channel_id: "C0123456789",
          upload_enabled: false,
          download_enabled: true,
          history_enabled: false
        )

        post "/api/v1/roles/#{role.oid}/slack_channel_permissions",
             params: { data: { channel_id: permission.channel_id, history_enabled: true } }.to_json,
             headers: auth_headers
        assert_response :ok

        permission.reload
        assert_not permission.upload_enabled
        assert_predicate permission, :download_enabled
        assert_predicate permission, :history_enabled
      end

      test "PUT by an unknown opaque id returns 404" do
        put api_v1_role_url(id: "role_nope"), params: { data: { name: "x" } }.to_json, headers: auth_headers
        assert_response :not_found
      end

      test "PUT upserts a new role by foreign_id" do
        body = { data: { namespace: "acme", name: "Edge" } }
        assert_difference -> { Role.count } => 1 do
          put api_v1_role_url(id: "edge"), params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_match(/\Arole_/, data["id"])
        assert_equal "acme", data["namespace"]
        assert_equal "edge", data["foreign_id"]
        assert_equal "Edge", data["name"]
      end

      test "PUT by foreign_id updates an existing role without creating" do
        role = roles(:acme_infra)
        body = { data: { namespace: "acme", name: "Renamed" } }
        assert_no_difference -> { Role.count } do
          put api_v1_role_url(id: "infra"), params: body.to_json, headers: auth_headers
        end
        assert_response :ok
        assert_equal "Renamed", role.reload.name
      end

      test "PUT upsert defaults the namespace when omitted" do
        body = { data: { name: "Defaulted" } }
        put api_v1_role_url(id: "defaulted"), params: body.to_json, headers: auth_headers
        assert_response :created
        assert_equal "default", json_body.dig("data", "namespace")
      end

      test "DELETE removes a role" do
        role = roles(:acme_admin_role)
        assert_difference -> { Role.count } => -1 do
          delete api_v1_role_url(id: role.oid), headers: auth_headers
        end
        assert_response :no_content
      end

      test "GET index lists roles in a namespace" do
        get api_v1_roles_url, params: { namespace: "acme" }, headers: auth_headers
        assert_response :ok
        foreign_ids = json_body.fetch("data").map { |r| r["foreign_id"] }
        assert_equal %w[admin infra].sort, foreign_ids.sort
      end

      test "GET index requires a namespace" do
        get api_v1_roles_url, headers: auth_headers
        assert_response :bad_request
      end

      test "GET lookup finds a role by namespace and foreign_id" do
        get lookup_api_v1_roles_url(namespace: "acme", foreign_id: "infra"), headers: auth_headers
        assert_response :ok
        assert_equal roles(:acme_infra).oid, json_body.dig("data", "id")
      end

      test "GET lookup returns 404 when nothing matches" do
        get lookup_api_v1_roles_url(namespace: "acme", foreign_id: "nope"), headers: auth_headers
        assert_response :not_found
      end
    end
  end
end
