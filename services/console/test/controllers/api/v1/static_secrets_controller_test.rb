require "test_helper"

module Api
  module V1
    class StaticSecretsControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers(token = ACME_TOKEN)
        { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      test "rejects requests without an Authorization header" do
        get api_v1_static_secret_url(id: "ssr_unknown")
        assert_response :unauthorized
        assert_equal "invalid or missing API key", json_body.dig("error", "message")
      end

      test "rejects requests with an unknown bearer token" do
        get api_v1_static_secret_url(id: "ssr_unknown"),
            headers: auth_headers("iak_not-a-real-token")
        assert_response :unauthorized
      end

      test "rejects requests with a malformed Authorization scheme" do
        get api_v1_static_secret_url(id: "ssr_unknown"),
            headers: { "Authorization" => "Token #{ACME_TOKEN}" }
        assert_response :unauthorized
      end

      test "GET returns a SecretRef with its source and rules" do
        ref = static_secrets(:github_token_inject)
        SecretSource.create!(source_type: "env", config: { "var" => "GITHUB_TOKEN" },
                              static_secret: ref)
        RequestRule.create!(host: "api.github.com", http_methods: %w[GET POST],
                             paths: [ "/" ], position: 0, static_secret: ref)

        get api_v1_static_secret_url(id: ref.oid), headers: auth_headers
        assert_response :ok

        data = json_body.fetch("data")
        assert_equal ref.oid, data["id"]
        assert_equal ref.namespace, data["namespace"]
        assert_equal ref.name, data["name"]
        assert_equal({ "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
                     data["inject_config"])
        assert_equal "env", data.dig("source", "source_type")
        assert_equal({ "var" => "GITHUB_TOKEN" }, data.dig("source", "config"))
        assert_nil data.dig("source", "id"), "source should not expose its own id"
        assert_equal 1, data["rules"].length
        rule = data["rules"].first
        assert_equal "api.github.com", rule["host"]
        assert_equal %w[GET POST], rule["http_methods"]
        assert_nil rule["id"], "rule should not expose its own id"
      end

      test "GET returns 404 for an unknown oid" do
        get api_v1_static_secret_url(id: "ssr_nope"), headers: auth_headers
        assert_response :not_found
      end

      test "GET lookup finds a static secret by namespace and foreign_id" do
        ref = static_secrets(:acme_prod_api_key)
        get lookup_api_v1_static_secrets_url(namespace: ref.namespace, foreign_id: ref.foreign_id),
            headers: auth_headers
        assert_response :ok
        assert_equal ref.oid, json_body.dig("data", "id")
      end

      test "GET lookup scopes a static secret by namespace" do
        ref = static_secrets(:acme_prod_api_key)
        get lookup_api_v1_static_secrets_url(namespace: "globex", foreign_id: ref.foreign_id),
            headers: auth_headers
        assert_response :not_found
      end

      test "GET lookup returns 404 when no static secret matches" do
        get lookup_api_v1_static_secrets_url(namespace: "acme", foreign_id: "does-not-exist"),
            headers: auth_headers
        assert_response :not_found
      end

      test "POST creates a SecretRef with nested source and rules in a single transaction" do
        body = {
          data: {
            namespace: "acme",
            name: "api-created-ref",
            description: "from API",
            labels: { "team" => "platform" },
            inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
            source: { source_type: "env", config: { "var" => "API_TOKEN" } },
            rules: [
              { host: "api.example.com", http_methods: [ "GET" ], paths: [ "/" ] },
              { host: "api2.example.com", http_methods: [ "POST" ], paths: [ "/v1" ] }
            ]
          }
        }

        assert_difference -> { StaticSecret.count } => 1,
                          -> { SecretSource.count } => 1,
                          -> { RequestRule.count } => 2 do
          post api_v1_static_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_match(/\Assr_/, data["id"])
        assert_equal "api-created-ref", data["name"]
        assert_equal "env", data.dig("source", "source_type")
        assert_equal [ 0, 1 ], data["rules"].map { |r| r["position"] }
      end

      test "POST ignores client-supplied rule positions and uses the array index" do
        body = {
          data: {
            namespace: "acme",
            name: "position-override",
            inject_config: { "header" => "Authorization" },
            source: { source_type: "env", config: { "var" => "X" } },
            rules: [
              { host: "a.example.com", http_methods: [ "GET" ], paths: [ "/" ], position: 99 },
              { host: "b.example.com", http_methods: [ "GET" ], paths: [ "/" ], position: 42 }
            ]
          }
        }

        post api_v1_static_secrets_url, params: body.to_json, headers: auth_headers
        assert_response :created

        data = json_body.fetch("data")
        assert_equal [ "a.example.com", "b.example.com" ], data["rules"].map { |r| r["host"] }
        assert_equal [ 0, 1 ], data["rules"].map { |r| r["position"] }
      end

      test "POST returns 422 when SSR validation fails (both inject and replace configs)" do
        body = {
          data: {
            namespace: "acme",
            name: "invalid-ref",
            inject_config: { "header" => "Authorization" },
            replace_config: { "proxy_value" => "__TOKEN__" }
          }
        }

        assert_no_difference -> { StaticSecret.count } do
          post api_v1_static_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_content
        assert_equal "validation failed", json_body.dig("error", "message")
      end

      test "POST returns 422 and rolls back SSR when a nested rule is invalid" do
        body = {
          data: {
            namespace: "acme",
            name: "rolled-back",
            inject_config: { "header" => "Authorization" },
            source: { source_type: "env", config: { "var" => "X" } },
            rules: [ { host: "good.example.com", http_methods: [ "GET" ], paths: [ "/" ] },
                     { http_methods: [ "GET" ], paths: [ "/" ] } ]
          }
        }

        assert_no_difference [ "StaticSecret.count", "SecretSource.count", "RequestRule.count" ] do
          post api_v1_static_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_content
      end

      test "POST rejects a foreign_id that starts with the opaque id prefix" do
        body = { data: { namespace: "acme", foreign_id: "ssr_collide", name: "x",
                         inject_config: { "header" => "X" } } }
        assert_no_difference -> { StaticSecret.count } do
          post api_v1_static_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_content
      end

      test "PUT upserts a new secret by foreign_id" do
        body = {
          data: {
            namespace: "acme",
            name: "upserted",
            inject_config: { "header" => "Authorization" },
            source: { source_type: "env", config: { "var" => "UP" } },
            rules: [ { host: "up.example.com", http_methods: [ "GET" ], paths: [ "/" ] } ]
          }
        }

        assert_difference -> { StaticSecret.count } => 1 do
          put api_v1_static_secret_url(id: "upserted-ref"), params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_equal "upserted-ref", data["foreign_id"]
        assert_equal "acme", data["namespace"]
        assert_equal "upserted", data["name"]
      end

      test "PUT by foreign_id updates an existing secret without creating" do
        ref = static_secrets(:acme_prod_api_key)
        body = {
          data: {
            namespace: ref.namespace,
            name: "renamed-by-upsert",
            inject_config: { "header" => "Authorization" },
            source: { source_type: "env", config: { "var" => "X" } },
            rules: [ { host: "x.example.com", http_methods: [ "GET" ], paths: [ "/" ] } ]
          }
        }

        assert_no_difference -> { StaticSecret.count } do
          put api_v1_static_secret_url(id: ref.foreign_id), params: body.to_json, headers: auth_headers
        end
        assert_response :ok
        assert_equal "renamed-by-upsert", ref.reload.name
      end

      test "POST returns 400 when the data key is missing" do
        post api_v1_static_secrets_url, params: { namespace: "acme" }.to_json, headers: auth_headers
        assert_response :bad_request
      end

      test "PUT updates SSR fields and replaces source and rules" do
        ref = static_secrets(:github_token_inject)
        SecretSource.create!(source_type: "env", config: { "var" => "OLD" }, static_secret: ref)
        old_rule = RequestRule.create!(host: "old.example.com", http_methods: [ "GET" ],
                                       paths: [ "/" ], position: 0, static_secret: ref)

        body = {
          data: {
            namespace: ref.namespace,
            name: ref.name,
            description: "updated",
            inject_config: { "header" => "X-New" },
            source: { source_type: "env", config: { "var" => "NEW" } },
            rules: [ { host: "new.example.com", http_methods: [ "POST" ], paths: [ "/v2" ] } ]
          }
        }

        put api_v1_static_secret_url(id: ref.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        ref.reload
        assert_equal "updated", ref.description
        assert_equal({ "header" => "X-New" }, ref.inject_config)
        assert_equal "NEW", ref.source.config["var"]
        assert_equal [ "new.example.com" ], ref.rules.map(&:host)
        assert_nil RequestRule.find_by(id: old_rule.id), "old rule should be deleted"
      end

      test "PUT switches a secret from replace_config to inject_config" do
        ref = static_secrets(:db_password_replace)

        body = {
          data: {
            namespace: ref.namespace,
            name: ref.name,
            inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
            replace_config: nil,
            source: { source_type: "env", config: { "var" => "DB_PASSWORD" } },
            rules: [ { host: "db.example.com" } ]
          }
        }

        put api_v1_static_secret_url(id: ref.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        ref.reload
        assert_equal({ "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
                     ref.inject_config)
        assert_nil ref.replace_config
      end

      test "PUT clears fields omitted from the body" do
        ref = static_secrets(:db_password_replace)

        body = {
          data: {
            namespace: ref.namespace,
            name: ref.name,
            inject_config: { "header" => "Authorization" }
          }
        }

        put api_v1_static_secret_url(id: ref.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        ref.reload
        assert_nil ref.replace_config
        assert_nil ref.description
        assert_equal({}, ref.labels)
      end

      test "PUT rolls back changes when validation fails" do
        ref = static_secrets(:github_token_inject)
        SecretSource.create!(source_type: "env", config: { "var" => "ORIGINAL" }, static_secret: ref)

        body = {
          data: {
            namespace: ref.namespace,
            name: ref.name,
            description: "should not persist",
            inject_config: { "header" => "Authorization" },
            replace_config: { "proxy_value" => "__X__" },
            source: { source_type: "env", config: { "var" => "WOULD_BE_NEW" } }
          }
        }

        put api_v1_static_secret_url(id: ref.oid), params: body.to_json, headers: auth_headers
        assert_response :unprocessable_content

        ref.reload
        assert_not_equal "should not persist", ref.description
        assert_equal "ORIGINAL", ref.source.config["var"]
      end

      test "PUT returns 404 for an unknown oid" do
        put api_v1_static_secret_url(id: "ssr_nope"),
            params: { data: { namespace: "acme", name: "x", inject_config: { "header" => "X" } } }.to_json,
            headers: auth_headers
        assert_response :not_found
      end

      test "GET index rejects requests without an Authorization header" do
        get api_v1_static_secrets_url, params: { namespace: "acme" }
        assert_response :unauthorized
      end

      test "GET index returns 400 when namespace is missing" do
        get api_v1_static_secrets_url, headers: auth_headers
        assert_response :bad_request
      end

      test "GET index returns all secret_refs in a namespace" do
        get api_v1_static_secrets_url, params: { namespace: "acme" }, headers: auth_headers
        assert_response :ok

        body = json_body
        names = body.fetch("data").map { |r| r["name"] }
        expected = StaticSecret.where(namespace: "acme").pluck(:name)
        assert_equal expected.sort, names.sort
        assert body["data"].all? { |r| r["namespace"] == "acme" }
        assert_equal expected.length, body.dig("meta", "total")
      end

      test "GET index filters by a single label" do
        get api_v1_static_secrets_url,
            params: { namespace: "acme", labels: { env: "prod" } },
            headers: auth_headers
        assert_response :ok

        names = json_body.fetch("data").map { |r| r["name"] }
        assert_equal %w[prod-api-key], names
      end

      test "GET index ANDs multiple label filters" do
        get api_v1_static_secrets_url,
            params: { namespace: "acme", labels: { team: "platform", env: "staging" } },
            headers: auth_headers
        assert_response :ok

        names = json_body.fetch("data").map { |r| r["name"] }
        assert_equal %w[staging-api-key], names
      end

      test "GET index does not leak across namespaces" do
        get api_v1_static_secrets_url,
            params: { namespace: "acme", labels: { team: "platform", env: "prod" } },
            headers: auth_headers
        assert_response :ok

        assert json_body.fetch("data").none? { |r| r["namespace"] == "globex" }
        assert_equal %w[prod-api-key], json_body.fetch("data").map { |r| r["name"] }
      end

      test "GET index returns an empty array when no labels match" do
        get api_v1_static_secrets_url,
            params: { namespace: "acme", labels: { env: "nowhere" } },
            headers: auth_headers
        assert_response :ok
        assert_equal [], json_body.fetch("data")
      end

      test "GET index honors limit and page" do
        total = StaticSecret.where(namespace: "acme").count

        get api_v1_static_secrets_url,
            params: { namespace: "acme", limit: 1, page: 2 },
            headers: auth_headers
        assert_response :ok

        body = json_body
        assert_equal 1, body.fetch("data").length
        assert_equal 1, body.dig("meta", "limit")
        assert_equal 2, body.dig("meta", "page")
        assert_equal total, body.dig("meta", "total")
        assert_equal total, body.dig("meta", "total_pages")
      end

      test "POST creates a control_plane source with an encrypted secret and never returns it" do
        body = {
          data: {
            namespace: "acme",
            name: "control-plane-ref",
            inject_config: { "header" => "Authorization" },
            source: { source_type: "control_plane", secret: "plaintext-secret" }
          }
        }

        assert_difference -> { StaticSecret.count } => 1,
                          -> { SecretSource.count } => 1 do
          post api_v1_static_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :created

        data = json_body.fetch("data")
        assert_equal "control_plane", data.dig("source", "source_type")
        assert_not response.body.include?("plaintext-secret"), "secret must not appear in response"
        assert_nil data.dig("source", "secret")

        ref = StaticSecret.find_by_oid!(data["id"])
        assert_equal "plaintext-secret", ref.source.secret

        get api_v1_static_secret_url(id: ref.oid), headers: auth_headers
        assert_response :ok
        assert_not response.body.include?("plaintext-secret"), "secret must not appear in GET response"
      end

      test "PUT rotates a control_plane source secret without exposing it" do
        ref = static_secrets(:github_token_inject)
        SecretSource.create!(source_type: "control_plane", secret: "old-secret", static_secret: ref)

        body = {
          data: {
            namespace: ref.namespace,
            name: ref.name,
            inject_config: { "header" => "Authorization" },
            source: { source_type: "control_plane", secret: "new-secret" }
          }
        }

        put api_v1_static_secret_url(id: ref.oid), params: body.to_json, headers: auth_headers
        assert_response :ok

        assert_not response.body.include?("new-secret"), "rotated secret must not appear in response"
        assert_equal "new-secret", ref.reload.source.secret
      end

      test "POST rejects a control_plane source without a secret" do
        body = {
          data: {
            namespace: "acme",
            name: "control-plane-no-secret",
            inject_config: { "header" => "Authorization" },
            source: { source_type: "control_plane" }
          }
        }

        assert_no_difference [ "StaticSecret.count", "SecretSource.count" ] do
          post api_v1_static_secrets_url, params: body.to_json, headers: auth_headers
        end
        assert_response :unprocessable_content
      end

      test "GET index clamps limit above the max" do
        get api_v1_static_secrets_url,
            params: { namespace: "acme", limit: 9999 },
            headers: auth_headers
        assert_response :ok
        assert_equal 200, json_body.dig("meta", "limit")
      end

      test "DELETE removes a static secret and its grants without deleting grantees" do
        secret = static_secrets(:github_token_inject)
        principal_grant = grants(:acme_channel_github_token)

        assert_difference -> { StaticSecret.count } => -1, -> { Grant.count } => -1 do
          delete api_v1_static_secret_url(id: secret.oid), headers: auth_headers
        end
        assert_response :no_content
        assert_nil StaticSecret.find_by_oid(secret.oid)
        refute Grant.exists?(principal_grant.id)
        # The principal that held the grant is untouched.
        assert Principal.exists?(principals(:acme_channel).id)
      end

      test "DELETE drops a role association when the secret is granted to a role" do
        secret = static_secrets(:acme_prod_api_key)
        role_grant = grants(:acme_infra_prod_api_key)

        assert_difference -> { Grant.count } => -1 do
          delete api_v1_static_secret_url(id: secret.oid), headers: auth_headers
        end
        assert_response :no_content
        refute Grant.exists?(role_grant.id)
        # The role that held the grant is untouched.
        assert Role.exists?(roles(:acme_infra).id)
      end

      test "DELETE returns 404 for an unknown static secret" do
        delete api_v1_static_secret_url(id: "ssr_nope"), headers: auth_headers
        assert_response :not_found
      end
    end
  end
end
