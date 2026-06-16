require "test_helper"

module Api
  module V1
    class ApiKeysControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze
      GLOBEX_TOKEN = "iak_globex-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      test "rejects requests without an Authorization header" do
        get api_v1_api_keys_url
        assert_response :unauthorized
        assert_equal "invalid or missing API key", json_body.dig("error", "message")
      end

      test "rejects requests with an unknown bearer token" do
        get api_v1_api_keys_url, headers: auth_headers("iak_not-a-real-token")
        assert_response :unauthorized
      end

      test "rejects requests with a malformed Authorization scheme" do
        get api_v1_api_keys_url, headers: { "Authorization" => "Token #{ACME_TOKEN}" }
        assert_response :unauthorized
      end

      test "GET index lists only the caller's keys and never returns plaintext token" do
        get api_v1_api_keys_url, headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        meta = json_body.fetch("meta")

        ids = data.map { |d| d["id"] }
        assert_includes ids, api_keys(:acme_ci_key).oid
        assert_includes ids, api_keys(:acme_extra_key).oid
        refute_includes ids, api_keys(:globex_ci_key).oid

        data.each { |d| refute d.key?("token"), "index payload must not include plaintext token" }
        assert_equal 2, meta["total"]
        assert_equal 1, meta["page"]
        assert_equal 50, meta["limit"]
        assert_equal 1, meta["total_pages"]
      end

      test "GET show returns the caller's key without token" do
        key = api_keys(:acme_extra_key)
        get api_v1_api_key_url(id: key.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal key.oid, data["id"]
        assert_equal "deploy", data["name"]
        refute data.key?("token")
      end

      test "GET show returns 404 for an unknown oid" do
        get api_v1_api_key_url(id: "ak_nope"), headers: auth_headers
        assert_response :not_found
      end

      test "GET show returns 404 for a key belonging to another user" do
        get api_v1_api_key_url(id: api_keys(:globex_ci_key).oid), headers: auth_headers
        assert_response :not_found
      end

      test "POST creates a key and returns the plaintext token once" do
        body = { data: { name: "ci-runner" } }

        assert_difference -> { ApiKey.count } => 1 do
          post api_v1_api_keys_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_match(/\Aak_/, data["id"])
        assert_equal "ci-runner", data["name"]
        token = data["token"]
        assert_match ApiKey::TOKEN_FORMAT, token

        found = ApiKey.find_by_token(token)
        refute_nil found
        assert_equal data["id"], found.oid
        assert_equal users(:acme_admin), found.user
      end

      test "POST without a name returns 422 with validation details" do
        body = { data: { name: "" } }
        assert_no_difference -> { ApiKey.count } do
          post api_v1_api_keys_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
        assert_equal "validation failed", json_body.dig("error", "message")
        assert json_body.dig("error", "details", "name").present?
      end

      test "DELETE soft-deletes the key" do
        key = api_keys(:acme_extra_key)

        delete api_v1_api_key_url(id: key.oid), headers: auth_headers
        assert_response :no_content

        # Row still exists (soft delete only sets deleted_at; default_scope hides it)
        row = ApiKey.unscoped.find(key.id)
        assert_not_nil row.deleted_at
        assert_nil ApiKey.find_by(id: key.id)
      end

      test "DELETE refuses to revoke the key used to authenticate the request" do
        key = api_keys(:acme_ci_key)

        delete api_v1_api_key_url(id: key.oid), headers: auth_headers
        assert_response :unprocessable_entity
        assert_match(/cannot revoke/, json_body.dig("error", "message"))

        assert_nil ApiKey.unscoped.find(key.id).deleted_at
      end

      test "DELETE on another user's key returns 404" do
        key = api_keys(:globex_ci_key)
        delete api_v1_api_key_url(id: key.oid), headers: auth_headers
        assert_response :not_found

        assert_nil ApiKey.unscoped.find(key.id).deleted_at
      end
    end
  end
end
