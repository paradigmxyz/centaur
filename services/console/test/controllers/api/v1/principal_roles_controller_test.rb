require "test_helper"

module Api
  module V1
    class PrincipalRolesControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      test "rejects unauthenticated requests" do
        get api_v1_principal_roles_url(principal_id: principals(:acme_channel).oid)
        assert_response :unauthorized
      end

      test "GET lists the roles assigned to a principal" do
        principal = principals(:acme_channel)
        get api_v1_principal_roles_url(principal_id: principal.oid), headers: auth_headers
        assert_response :ok

        ids = json_body.fetch("data").map { |r| r["id"] }
        assert_equal [ roles(:acme_infra).oid ], ids
      end

      test "POST assigns a role to a principal" do
        principal = principals(:acme_user_bob)
        role = roles(:acme_admin_role)
        body = { data: { role_id: role.oid } }

        assert_difference -> { principal.principal_roles.count } => 1 do
          post api_v1_principal_roles_url(principal_id: principal.oid),
               params: body.to_json, headers: auth_headers
        end
        assert_response :created
        assert_equal role.oid, json_body.dig("data", "id")
      end

      test "POST returns 422 when the role is in a different namespace" do
        principal = principals(:globex_user)
        body = { data: { role_id: roles(:acme_infra).oid } }

        assert_no_difference -> { principal.principal_roles.count } do
          post api_v1_principal_roles_url(principal_id: principal.oid),
               params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_content
      end

      test "POST is idempotent when the role is already assigned" do
        principal = principals(:acme_channel)
        role = roles(:acme_infra)
        body = { data: { role_id: role.oid } }

        assert_no_difference -> { principal.principal_roles.count } do
          post api_v1_principal_roles_url(principal_id: principal.oid),
               params: body.to_json, headers: auth_headers
        end
        assert_response :ok
        assert_equal role.oid, json_body.dig("data", "id")
      end

      test "POST returns 404 for an unknown role" do
        post api_v1_principal_roles_url(principal_id: principals(:acme_channel).oid),
             params: { data: { role_id: "role_nope" } }.to_json, headers: auth_headers
        assert_response :not_found
      end

      test "DELETE unassigns a role" do
        principal = principals(:acme_channel)
        role = roles(:acme_infra)

        assert_difference -> { principal.principal_roles.count } => -1 do
          delete api_v1_principal_role_url(principal_id: principal.oid, id: role.oid),
                 headers: auth_headers
        end
        assert_response :no_content
      end

      test "DELETE returns 404 when the role is not assigned" do
        principal = principals(:acme_user_bob)
        delete api_v1_principal_role_url(principal_id: principal.oid, id: roles(:acme_admin_role).oid),
               headers: auth_headers
        assert_response :not_found
      end
    end
  end
end
