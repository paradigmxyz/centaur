require "test_helper"

module Api
  module V1
    class HmacSecretsControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      def valid_body(overrides = {})
        {
          data: {
            namespace: "acme",
            foreign_id: "new-hmac",
            timestamp_format: "unix_seconds",
            signature_algorithm: "sha256",
            signature_key_encoding: "hex",
            signature_output_encoding: "base64",
            signature_message: "{{ .Timestamp }}.{{ .Body }}",
            headers: [ { name: "X-Signature", value: "{{ .Signature }}" } ],
            credentials: { secret: { source_type: "env", config: { var: "HMAC_KEY" } } },
            rules: [ { host: "hooks.example.com", http_methods: [ "POST" ] } ]
          }.merge(overrides)
        }
      end

      test "rejects requests without an Authorization header" do
        get api_v1_hmac_secret_url(id: "hms_unknown")
        assert_response :unauthorized
      end

      test "GET returns an hmac secret with its credentials and rules" do
        secret = hmac_secrets(:acme_webhook_hmac)
        get api_v1_hmac_secret_url(id: secret.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal "sha256", data["signature_algorithm"]
        assert_equal({ "source_type" => "env", "config" => { "var" => "WEBHOOK_HMAC_KEY" } },
                     data.dig("credentials", "secret"))
        assert_equal "X-Signature", data.dig("headers", 0, "name")
        assert_equal 1, data["rules"].length
      end

      test "GET lookup finds an hmac secret by namespace and foreign_id" do
        secret = hmac_secrets(:acme_webhook_hmac)
        get lookup_api_v1_hmac_secrets_url(namespace: secret.namespace, foreign_id: secret.foreign_id),
            headers: auth_headers
        assert_response :ok
        assert_equal secret.oid, json_body.dig("data", "id")
      end

      test "GET lookup scopes an hmac secret by namespace" do
        secret = hmac_secrets(:acme_webhook_hmac)
        get lookup_api_v1_hmac_secrets_url(namespace: "globex", foreign_id: secret.foreign_id),
            headers: auth_headers
        assert_response :not_found
      end

      test "GET lookup returns 404 when no hmac secret matches" do
        get lookup_api_v1_hmac_secrets_url(namespace: "acme", foreign_id: "does-not-exist"),
            headers: auth_headers
        assert_response :not_found
      end

      test "POST creates an hmac secret" do
        assert_difference -> { HmacSecret.count } => 1 do
          post api_v1_hmac_secrets_url, params: valid_body.to_json, headers: auth_headers
        end
        assert_response :created

        secret = HmacSecret.find_by_oid(json_body.dig("data", "id"))
        assert_equal %w[secret], secret.sources.map(&:role)
        assert_equal 1, secret.rules.count
        assert_equal "{{ .Signature }}", secret.headers.first["value"]
      end

      test "POST rejects a secret missing the required credential" do
        body = valid_body(credentials: { key_id: { source_type: "env", config: { var: "KID" } } })

        assert_no_difference -> { HmacSecret.count } do
          post api_v1_hmac_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "POST rejects an unknown signature algorithm" do
        body = valid_body(signature_algorithm: "md5")

        assert_no_difference -> { HmacSecret.count } do
          post api_v1_hmac_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "PUT upserts a new hmac secret by foreign_id" do
        body = valid_body
        body[:data].delete(:foreign_id)

        assert_difference -> { HmacSecret.count } => 1 do
          put api_v1_hmac_secret_url(id: "hmac-upsert"), params: body.to_json, headers: auth_headers
        end
        assert_response :created
        assert_equal "hmac-upsert", json_body.dig("data", "foreign_id")
      end

      test "PUT replaces credentials and rules" do
        secret = hmac_secrets(:acme_webhook_hmac)
        body = valid_body(credentials: {
          secret: { source_type: "env", config: { var: "NEW_KEY" } },
          key_id: { source_type: "env", config: { var: "NEW_KID" } }
        })
        body[:data].delete(:foreign_id)

        put api_v1_hmac_secret_url(id: secret.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        secret.reload
        assert_equal %w[key_id secret].sort, secret.sources.map(&:role).sort
      end

      test "PUT clears fields omitted from the body" do
        secret = hmac_secrets(:acme_webhook_hmac)
        body = valid_body
        body[:data].delete(:foreign_id)

        put api_v1_hmac_secret_url(id: secret.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        secret.reload
        assert_equal({}, secret.labels)
        assert_nil secret.name
      end

      test "GET index is scoped by namespace" do
        get api_v1_hmac_secrets_url, params: { namespace: "acme" }, headers: auth_headers
        assert_response :ok
        ids = json_body.fetch("data").map { |r| r["id"] }
        assert_includes ids, hmac_secrets(:acme_webhook_hmac).oid
      end

      test "DELETE removes an hmac secret" do
        secret = hmac_secrets(:acme_webhook_hmac)
        assert_difference -> { HmacSecret.count } => -1 do
          delete api_v1_hmac_secret_url(id: secret.oid), headers: auth_headers
        end
        assert_response :no_content
        assert_nil HmacSecret.find_by_oid(secret.oid)
      end

      test "DELETE returns 404 for an unknown hmac secret" do
        delete api_v1_hmac_secret_url(id: "hms_nope"), headers: auth_headers
        assert_response :not_found
      end
    end
  end
end
