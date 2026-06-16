require "test_helper"

module Api
  module V1
    class GranteeGrantsControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      test "rejects requests without an Authorization header" do
        get api_v1_principal_grants_url(principal_id: principals(:acme_channel).oid)
        assert_response :unauthorized
        assert_equal "invalid or missing API key", json_body.dig("error", "message")
      end

      test "GET lists the grants for a principal grantee" do
        principal = principals(:acme_channel)

        get api_v1_principal_grants_url(principal_id: principal.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        returned = data.map { |g| g["id"] }.sort
        expected = principal.grants.map(&:oid).sort
        assert_equal expected, returned
        assert data.all? { |g| g["principal_id"] == principal.oid }
        assert_equal data.length, json_body.dig("meta", "total")
      end

      test "GET includes the grantable oid for each grant" do
        grant = grants(:acme_channel_github_token)

        get api_v1_principal_grants_url(principal_id: grant.principal.oid), headers: auth_headers
        assert_response :ok

        entry = json_body.fetch("data").find { |g| g["id"] == grant.oid }
        assert_equal grant.static_secret.oid, entry["static_secret_id"]
      end

      test "GET lists the grants for a role grantee" do
        role = roles(:acme_infra)
        grant = grants(:acme_infra_prod_api_key)

        get api_v1_role_grants_url(role_id: role.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal [ grant.oid ], data.map { |g| g["id"] }
        assert_equal role.oid, data.first["role_id"]
        assert_nil data.first["principal_id"]
      end

      test "GET returns an empty list for a grantee with no grants" do
        role = roles(:acme_admin_role)
        assert_empty role.grants

        get api_v1_role_grants_url(role_id: role.oid), headers: auth_headers
        assert_response :ok
        assert_empty json_body.fetch("data")
        assert_equal 0, json_body.dig("meta", "total")
      end

      test "GET returns 404 for an unknown principal grantee" do
        get api_v1_principal_grants_url(principal_id: "prn_nope"), headers: auth_headers
        assert_response :not_found
      end

      test "GET returns 404 for an unknown role grantee" do
        get api_v1_role_grants_url(role_id: "role_nope"), headers: auth_headers
        assert_response :not_found
      end

      test "GET paginates results" do
        principal = principals(:acme_channel)
        assert_operator principal.grants.count, :>, 1

        get api_v1_principal_grants_url(principal_id: principal.oid, limit: 1, page: 1),
            headers: auth_headers
        assert_response :ok

        assert_equal 1, json_body.fetch("data").length
        meta = json_body.fetch("meta")
        assert_equal 1, meta["limit"]
        assert_equal principal.grants.count, meta["total"]
        assert_equal principal.grants.count, meta["total_pages"]
      end
    end
  end
end
