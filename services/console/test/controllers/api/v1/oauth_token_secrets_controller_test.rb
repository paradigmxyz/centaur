require "test_helper"

module Api
  module V1
    class OauthTokenSecretsControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      test "rejects requests without an Authorization header" do
        get api_v1_oauth_token_secret_url(id: "ots_unknown")
        assert_response :unauthorized
      end

      test "GET returns an oauth_token secret with its credentials and rules" do
        secret = oauth_token_secrets(:acme_gmail_oauth)
        get api_v1_oauth_token_secret_url(id: secret.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal "refresh_token", data["grant"]
        assert_equal({ "source_type" => "env", "config" => { "var" => "GMAIL_CLIENT_ID" } },
                     data.dig("credentials", "client_id"))
        assert data.dig("credentials", "refresh_token").present?
      end

      test "GET lookup finds an oauth_token secret by namespace and foreign_id" do
        secret = oauth_token_secrets(:acme_gmail_oauth)
        get lookup_api_v1_oauth_token_secrets_url(namespace: secret.namespace, foreign_id: secret.foreign_id),
            headers: auth_headers
        assert_response :ok
        assert_equal secret.oid, json_body.dig("data", "id")
      end

      test "GET lookup scopes an oauth_token secret by namespace" do
        secret = oauth_token_secrets(:acme_gmail_oauth)
        get lookup_api_v1_oauth_token_secrets_url(namespace: "globex", foreign_id: secret.foreign_id),
            headers: auth_headers
        assert_response :not_found
      end

      test "GET lookup returns 404 when no oauth_token secret matches" do
        get lookup_api_v1_oauth_token_secrets_url(namespace: "acme", foreign_id: "does-not-exist"),
            headers: auth_headers
        assert_response :not_found
      end

      test "POST creates a refresh_token oauth secret" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "new-oauth",
            grant: "refresh_token",
            token_endpoint: "https://oauth2.googleapis.com/token",
            scopes: [ "gmail.readonly" ],
            credentials: {
              refresh_token: { source_type: "env", config: { var: "RT" } },
              client_id: { source_type: "env", config: { var: "CID" } }
            },
            rules: [ { host: "gmail.googleapis.com", http_methods: [ "GET" ] } ]
          }
        }

        assert_difference -> { OauthTokenSecret.count } => 1 do
          post api_v1_oauth_token_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        secret = OauthTokenSecret.find_by_oid(json_body.dig("data", "id"))
        assert_equal %w[client_id refresh_token].sort, secret.sources.map(&:role).sort
        assert_equal 1, secret.rules.count
      end

      test "POST creates a password grant with token_endpoint_headers" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "password-provider",
            grant: "password",
            token_endpoint: "https://auth.example.com/token",
            credentials: {
              username: { source_type: "env", config: { var: "PROVIDER_USER" } },
              password: { source_type: "env", config: { var: "PROVIDER_PASS" } },
              client_id: { source_type: "env", config: { var: "PROVIDER_CID" } }
            },
            token_endpoint_headers: {
              "x-api-key": { source_type: "env", config: { var: "PROVIDER_KEY" } }
            },
            rules: [ { host: "api.example.com" } ]
          }
        }

        post api_v1_oauth_token_secrets_url, params: body.to_json, headers: auth_headers
        assert_response :created

        secret = OauthTokenSecret.find_by_oid(json_body.dig("data", "id"))
        header = secret.sources.find(&:endpoint_header?)
        assert_equal "x-api-key", header.role
        assert_equal({ "x-api-key" => { "source_type" => "env", "config" => { "var" => "PROVIDER_KEY" } } },
                     json_body.dig("data", "token_endpoint_headers"))
      end

      test "POST rejects a grant that is missing a required field" do
        body = {
          data: {
            namespace: "acme",
            foreign_id: "incomplete",
            grant: "refresh_token",
            token_endpoint: "https://oauth2.googleapis.com/token",
            credentials: { refresh_token: { source_type: "env", config: { var: "RT" } } },
            rules: [ { host: "gmail.googleapis.com" } ]
          }
        }

        assert_no_difference -> { OauthTokenSecret.count } do
          post api_v1_oauth_token_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_entity
      end

      test "PUT replaces credentials and rules" do
        secret = oauth_token_secrets(:acme_gmail_oauth)
        body = {
          data: {
            grant: "client_credentials",
            token_endpoint: "https://oauth2.googleapis.com/token",
            credentials: {
              client_id: { source_type: "env", config: { var: "NEW_CID" } },
              client_secret: { source_type: "env", config: { var: "NEW_SECRET" } }
            },
            rules: [ { host: "www.googleapis.com" } ]
          }
        }

        put api_v1_oauth_token_secret_url(id: secret.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        secret.reload
        assert_equal "client_credentials", secret.grant
        assert_equal %w[client_id client_secret].sort, secret.sources.map(&:role).sort
        assert_equal 1, secret.rules.count
      end

      test "PUT clears a stale audience when omitted from the body" do
        secret = oauth_token_secrets(:acme_gmail_oauth)
        secret.update!(audience: "https://old.example.com")
        body = {
          data: {
            grant: "client_credentials",
            token_endpoint: "https://oauth2.googleapis.com/token",
            credentials: {
              client_id: { source_type: "env", config: { var: "CID" } },
              client_secret: { source_type: "env", config: { var: "SECRET" } }
            },
            rules: [ { host: "www.googleapis.com" } ]
          }
        }

        put api_v1_oauth_token_secret_url(id: secret.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        secret.reload
        assert_nil secret.audience
        assert_equal({}, secret.labels)
      end

      test "PUT upserts a new oauth secret by foreign_id" do
        body = {
          data: {
            namespace: "acme",
            grant: "client_credentials",
            token_endpoint: "https://oauth2.googleapis.com/token",
            credentials: {
              client_id: { source_type: "env", config: { var: "CID" } },
              client_secret: { source_type: "env", config: { var: "SECRET" } }
            },
            rules: [ { host: "www.googleapis.com" } ]
          }
        }

        assert_difference -> { OauthTokenSecret.count } => 1 do
          put api_v1_oauth_token_secret_url(id: "cc-upsert"), params: body.to_json, headers: auth_headers
        end
        assert_response :created
        assert_equal "cc-upsert", json_body.dig("data", "foreign_id")
      end

      test "GET index is scoped by namespace" do
        get api_v1_oauth_token_secrets_url, params: { namespace: "acme" }, headers: auth_headers
        assert_response :ok
        ids = json_body.fetch("data").map { |r| r["id"] }
        assert_includes ids, oauth_token_secrets(:acme_gmail_oauth).oid
      end

      test "DELETE removes an oauth_token secret" do
        secret = oauth_token_secrets(:acme_gmail_oauth)
        assert_difference -> { OauthTokenSecret.count } => -1 do
          delete api_v1_oauth_token_secret_url(id: secret.oid), headers: auth_headers
        end
        assert_response :no_content
        assert_nil OauthTokenSecret.find_by_oid(secret.oid)
      end

      test "DELETE returns 404 for an unknown oauth_token secret" do
        delete api_v1_oauth_token_secret_url(id: "ots_nope"), headers: auth_headers
        assert_response :not_found
      end
    end
  end
end
