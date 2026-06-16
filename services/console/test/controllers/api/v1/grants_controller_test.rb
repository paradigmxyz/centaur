require "test_helper"

module Api
  module V1
    class GrantsControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      test "rejects requests without an Authorization header" do
        get api_v1_grant_url(id: "grant_unknown")
        assert_response :unauthorized
        assert_equal "invalid or missing API key", json_body.dig("error", "message")
      end

      test "rejects requests with an unknown bearer token" do
        get api_v1_grant_url(id: "grant_unknown"),
            headers: auth_headers("iak_not-a-real-token")
        assert_response :unauthorized
      end

      test "rejects requests with a malformed Authorization scheme" do
        get api_v1_grant_url(id: "grant_unknown"),
            headers: { "Authorization" => "Token #{ACME_TOKEN}" }
        assert_response :unauthorized
      end

      test "GET returns a Grant with principal and secret ref OIDs" do
        grant = grants(:acme_channel_github_token)

        get api_v1_grant_url(id: grant.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal grant.oid, data["id"]
        assert_equal grant.principal.oid, data["principal_id"]
        assert_equal grant.static_secret.oid, data["static_secret_id"]
      end

      test "GET returns 404 for an unknown oid" do
        get api_v1_grant_url(id: "grant_nope"), headers: auth_headers
        assert_response :not_found
      end

      test "POST creates a Grant" do
        principal = principals(:globex_user)
        secret_ref = static_secrets(:github_token_inject)

        body = {
          data: {
            principal_id: principal.oid,
            static_secret_id: secret_ref.oid
          }
        }

        assert_difference -> { Grant.count } => 1 do
          post api_v1_grants_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_match(/\Agrant_/, data["id"])
        assert_equal principal.oid, data["principal_id"]
        assert_equal secret_ref.oid, data["static_secret_id"]
      end

      test "POST creates a Grant for a gcp_auth secret" do
        principal = principals(:globex_user)
        secret = gcp_auth_secrets(:acme_bigquery)

        body = { data: { principal_id: principal.oid, gcp_auth_secret_id: secret.oid } }

        assert_difference -> { Grant.count } => 1 do
          post api_v1_grants_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created
        assert_equal secret.oid, json_body.dig("data", "gcp_auth_secret_id")
      end

      test "POST creates a Grant for an oauth_token secret" do
        principal = principals(:globex_user)
        secret = oauth_token_secrets(:acme_gmail_oauth)

        body = { data: { principal_id: principal.oid, oauth_token_secret_id: secret.oid } }

        post api_v1_grants_url, params: body.to_json, headers: auth_headers
        assert_response :created
        assert_equal secret.oid, json_body.dig("data", "oauth_token_secret_id")
      end

      test "POST creates a Grant for an hmac secret" do
        principal = principals(:globex_user)
        secret = hmac_secrets(:acme_webhook_hmac)

        body = { data: { principal_id: principal.oid, hmac_secret_id: secret.oid } }

        post api_v1_grants_url, params: body.to_json, headers: auth_headers
        assert_response :created
        assert_equal secret.oid, json_body.dig("data", "hmac_secret_id")
      end

      test "POST creates a Grant for a role grantee" do
        role = roles(:acme_admin_role)
        secret_ref = static_secrets(:github_token_inject)
        body = { data: { role_id: role.oid, static_secret_id: secret_ref.oid } }

        assert_difference -> { Grant.count } => 1 do
          post api_v1_grants_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_equal role.oid, data["role_id"]
        assert_nil data["principal_id"]
        assert_equal secret_ref.oid, data["static_secret_id"]
      end

      test "POST is idempotent: re-granting the same secret to a role returns the existing grant" do
        role = roles(:acme_admin_role)
        secret_ref = static_secrets(:github_token_inject)
        body = { data: { role_id: role.oid, static_secret_id: secret_ref.oid } }

        post api_v1_grants_url, params: body.to_json, headers: auth_headers
        assert_response :created
        first_oid = json_body.dig("data", "id")

        assert_no_difference -> { Grant.count } do
          post api_v1_grants_url, params: body.to_json, headers: auth_headers
        end
        assert_response :ok
        assert_equal first_oid, json_body.dig("data", "id")
      end

      test "POST is idempotent: re-granting the same secret to a principal returns the existing grant" do
        principal = principals(:globex_user)
        secret_ref = static_secrets(:github_token_inject)
        body = { data: { principal_id: principal.oid, static_secret_id: secret_ref.oid } }

        post api_v1_grants_url, params: body.to_json, headers: auth_headers
        assert_response :created
        first_oid = json_body.dig("data", "id")

        assert_no_difference -> { Grant.count } do
          post api_v1_grants_url, params: body.to_json, headers: auth_headers
        end
        assert_response :ok
        assert_equal first_oid, json_body.dig("data", "id")
      end

      test "GET returns a role-granted Grant with its role_id" do
        grant = grants(:acme_infra_prod_api_key)
        get api_v1_grant_url(id: grant.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal grant.role.oid, data["role_id"]
        assert_nil data["principal_id"]
      end

      test "POST returns 422 when no grantee id is supplied" do
        body = { data: { static_secret_id: static_secrets(:github_token_inject).oid } }

        assert_no_difference -> { Grant.count } do
          post api_v1_grants_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "POST returns 404 when role_id is unknown" do
        body = { data: { role_id: "role_nope", static_secret_id: static_secrets(:github_token_inject).oid } }

        assert_no_difference -> { Grant.count } do
          post api_v1_grants_url, params: body.to_json, headers: auth_headers
        end
        assert_response :not_found
      end

      test "POST returns 422 when no grantable id is supplied" do
        principal = principals(:globex_user)
        body = { data: { principal_id: principal.oid } }

        assert_no_difference -> { Grant.count } do
          post api_v1_grants_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "POST returns 404 when principal_id is unknown" do
        secret_ref = static_secrets(:github_token_inject)
        body = {
          data: {
            principal_id: "prn_nope",
            static_secret_id: secret_ref.oid
          }
        }

        assert_no_difference -> { Grant.count } do
          post api_v1_grants_url, params: body.to_json, headers: auth_headers
        end
        assert_response :not_found
      end

      test "POST returns 404 when static_secret_id is unknown" do
        principal = principals(:globex_user)
        body = {
          data: {
            principal_id: principal.oid,
            static_secret_id: "ssr_nope"
          }
        }

        assert_no_difference -> { Grant.count } do
          post api_v1_grants_url, params: body.to_json, headers: auth_headers
        end
        assert_response :not_found
      end

      test "POST returns 400 when the data key is missing" do
        post api_v1_grants_url, params: { principal_id: "prn_x" }.to_json, headers: auth_headers
        assert_response :bad_request
      end

      test "DELETE removes the Grant" do
        grant = grants(:acme_channel_github_token)

        assert_difference -> { Grant.count } => -1 do
          delete api_v1_grant_url(id: grant.oid), headers: auth_headers
        end
        assert_response :no_content

        get api_v1_grant_url(id: grant.oid), headers: auth_headers
        assert_response :not_found
      end

      test "DELETE returns 404 for an unknown oid" do
        delete api_v1_grant_url(id: "grant_nope"), headers: auth_headers
        assert_response :not_found
      end
    end
  end
end
