require "test_helper"

module Api
  module V1
    class AwsAuthSecretsControllerTest < ActionDispatch::IntegrationTest
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
            foreign_id: "new-aws",
            allowed_services: [ "logs", "monitoring" ],
            access_key_id: { source_type: "env", config: { var: "AWS_ACCESS_KEY_ID" } },
            secret_access_key: { source_type: "env", config: { var: "AWS_SECRET_ACCESS_KEY" } },
            rules: [ { host: "logs.us-west-2.amazonaws.com", http_methods: [ "POST" ] } ]
          }.merge(overrides)
        }
      end

      test "rejects requests without an Authorization header" do
        get api_v1_aws_auth_secret_url(id: "aas_unknown")
        assert_response :unauthorized
      end

      test "GET returns an aws_auth secret with its credential sources and rules" do
        secret = aws_auth_secrets(:acme_cloudwatch_aws)
        get api_v1_aws_auth_secret_url(id: secret.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal({ "source_type" => "env", "config" => { "var" => "AWS_ACCESS_KEY_ID" } }, data["access_key_id"])
        assert_equal({ "source_type" => "env", "config" => { "var" => "AWS_SECRET_ACCESS_KEY" } }, data["secret_access_key"])
        assert_nil data["session_token"]
        assert_equal %w[logs monitoring], data["allowed_services"]
        assert_equal 1, data["rules"].length
      end

      test "GET lookup finds an aws_auth secret by namespace and foreign_id" do
        secret = aws_auth_secrets(:acme_cloudwatch_aws)
        get lookup_api_v1_aws_auth_secrets_url(namespace: secret.namespace, foreign_id: secret.foreign_id),
            headers: auth_headers
        assert_response :ok
        assert_equal secret.oid, json_body.dig("data", "id")
      end

      test "GET lookup returns 404 when no aws_auth secret matches" do
        get lookup_api_v1_aws_auth_secrets_url(namespace: "acme", foreign_id: "does-not-exist"),
            headers: auth_headers
        assert_response :not_found
      end

      test "POST creates an aws_auth secret" do
        assert_difference -> { AwsAuthSecret.count } => 1 do
          post api_v1_aws_auth_secrets_url, params: valid_body.to_json, headers: auth_headers
        end
        assert_response :created

        secret = AwsAuthSecret.find_by_oid(json_body.dig("data", "id"))
        assert_equal %w[access_key_id secret_access_key].sort, secret.sources.map(&:role).sort
        assert_equal 1, secret.rules.count
      end

      test "POST creates an aws_auth secret with a session_token" do
        body = valid_body(session_token: { source_type: "env", config: { var: "AWS_SESSION_TOKEN" } })
        post api_v1_aws_auth_secrets_url, params: body.to_json, headers: auth_headers
        assert_response :created
        secret = AwsAuthSecret.find_by_oid(json_body.dig("data", "id"))
        assert_includes secret.sources.map(&:role), "session_token"
      end

      test "POST rejects a secret missing the required secret_access_key" do
        body = valid_body
        body[:data].delete(:secret_access_key)

        assert_no_difference -> { AwsAuthSecret.count } do
          post api_v1_aws_auth_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "PUT upserts a new aws_auth secret by foreign_id" do
        body = valid_body
        body[:data].delete(:foreign_id)

        assert_difference -> { AwsAuthSecret.count } => 1 do
          put api_v1_aws_auth_secret_url(id: "aws-upsert"), params: body.to_json, headers: auth_headers
        end
        assert_response :created
        assert_equal "aws-upsert", json_body.dig("data", "foreign_id")
      end

      test "PUT replaces credential sources" do
        secret = aws_auth_secrets(:acme_cloudwatch_aws)
        body = valid_body(
          access_key_id: { source_type: "env", config: { var: "NEW_AK" } },
          secret_access_key: { source_type: "env", config: { var: "NEW_SK" } }
        )
        body[:data].delete(:foreign_id)

        put api_v1_aws_auth_secret_url(id: secret.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        secret.reload
        assert_equal "NEW_AK", secret.sources.find { |s| s.role == "access_key_id" }.config["var"]
      end

      test "PUT clears fields omitted from the body" do
        secret = aws_auth_secrets(:acme_cloudwatch_aws)
        body = valid_body
        body[:data].delete(:foreign_id)
        body[:data].delete(:allowed_services)

        put api_v1_aws_auth_secret_url(id: secret.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        secret.reload
        assert_equal({}, secret.labels)
        assert_equal [], secret.allowed_services
      end

      test "DELETE removes an aws_auth secret" do
        secret = aws_auth_secrets(:acme_cloudwatch_aws)
        assert_difference -> { AwsAuthSecret.count } => -1 do
          delete api_v1_aws_auth_secret_url(id: secret.oid), headers: auth_headers
        end
        assert_response :no_content
        assert_nil AwsAuthSecret.find_by_oid(secret.oid)
      end
    end
  end
end
