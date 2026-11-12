require "test_helper"

module Api
  module V1
    class ConsoleApiClientsTest < ActionDispatch::IntegrationTest
      setup do
        ENV["CENTAUR_CONSOLE_API_CLIENTS_ENABLED"] = "true"
        sign_in_console(users(:acme_admin))
      end

      teardown do
        ENV.delete("CENTAUR_CONSOLE_API_CLIENTS_ENABLED")
      end

      def create_client(name: "eval-runner")
        ApiClient.create!(name: name, scopes: ApiClient::SCOPES, created_by: users(:acme_admin))
      end

      test "API client endpoints are admin-only for every caller type" do
        client = create_client
        link = client.attach_principal!(principals(:acme_channel))
        digest = client.secret_digest

        assert_admin_only_api :get, api_v1_api_clients_path
        assert_admin_only_api :get, api_v1_api_client_path(client.oid)
        assert_admin_only_api :post, api_v1_api_clients_path, params: { data: { name: "x", scopes: ApiClient::SCOPES } }
        assert_admin_only_api :patch, api_v1_api_client_path(client.oid), params: { data: { name: "renamed" } }
        assert_admin_only_api :delete, api_v1_api_client_path(client.oid)
        assert_admin_only_api :post, rotate_secret_api_v1_api_client_path(client.oid)
        assert_admin_only_api :post, disable_api_v1_api_client_path(client.oid)
        assert_admin_only_api :post, enable_api_v1_api_client_path(client.oid)
        assert_admin_only_api :post, attach_principal_api_v1_api_client_path(client.oid),
                              params: { data: { principal: principals(:acme_user_bob).oid } }
        assert_admin_only_api :delete, detach_principal_api_v1_api_client_path(client.oid, link.oid)
        assert_admin_only_api :post, make_default_principal_api_v1_api_client_path(client.oid, link.oid)

        assert_equal 1, ApiClient.count
        client.reload
        assert_equal [ "eval-runner", digest, false ], [ client.name, client.secret_digest, client.disabled? ]
        assert_equal [ principals(:acme_channel) ], client.principals.to_a
      end

      test "API clients are unreachable while the feature is off, even before authentication" do
        client = create_client
        ENV.delete("CENTAUR_CONSOLE_API_CLIENTS_ENABLED")

        get api_v1_api_clients_path
        assert_response :not_found
        assert_equal({ "error" => { "message" => "not found" } }, json_body)
        post rotate_secret_api_v1_api_client_path(client.oid), headers: csrf_headers, as: :json
        assert_response :not_found
        assert_no_difference -> { ApiClient.count } do
          post api_v1_api_clients_path, params: { data: { name: "x", scopes: ApiClient::SCOPES } },
               headers: csrf_headers, as: :json
        end
        assert_response :not_found

        reset!
        get api_v1_api_clients_path
        assert_response :not_found
        get api_v1_api_clients_path, headers: bearer_headers(MEMBER_TOKEN)
        assert_response :not_found
      end

      test "me offers the API Clients section only while the feature is on" do
        get api_v1_me_path
        assert_equal true, json_body.dig("data", "nav", "api_clients")

        ENV.delete("CENTAUR_CONSOLE_API_CLIENTS_ENABLED")
        get api_v1_me_path
        assert_equal false, json_body.dig("data", "nav", "api_clients")
      end

      test "non-admins cannot manage API clients" do
        sign_in_console(users(:member_user))

        get api_v1_api_clients_path
        assert_response :forbidden

        assert_no_difference -> { ApiClient.count } do
          post api_v1_api_clients_path, params: { data: { name: "x", scopes: ApiClient::SCOPES } },
               headers: csrf_headers, as: :json
        end
        assert_response :forbidden
      end

      test "index lists clients with scopes, principal count, and status, never secrets" do
        client = create_client
        client.attach_principal!(principals(:acme_channel))
        create_client(name: "other").update!(disabled_at: Time.current)

        get api_v1_api_clients_path
        assert_response :ok
        assert_equal %w[eval-runner other], json_body["data"].map { |c| c["name"] }
        first = json_body["data"].first
        assert_equal client.oid, first["id"]
        assert_equal ApiClient::SCOPES, first["scopes"]
        assert_equal 1, first["principals_count"]
        assert_equal [ false, true ], json_body["data"].map { |c| c["disabled"] }
        assert_not_includes response.body, client.secret_digest
        assert json_body["data"].none? { |c| c.key?("secret") }
      end

      test "create shows the secret once with its default principal attached" do
        principal = principals(:acme_user_alice)

        post api_v1_api_clients_path, params: {
          data: { name: "eval-runner", scopes: [ "", "sessions:read" ], principal: principal.foreign_id }
        }, headers: csrf_headers, as: :json

        assert_response :created
        client = ApiClient.order(:id).last
        assert_equal %w[sessions:read], client.scopes
        assert_equal client.created_by, users(:acme_admin)
        assert_equal principal, client.default_principal_link.principal
        assert_equal "no-store", response.headers["Cache-Control"]
        secret = json_body.dig("data", "secret")
        assert secret.start_with?(ApiClient::SECRET_PREFIX)
        assert_equal client.secret_digest, ApiClient.digest_secret(secret)
        assert_equal [ [ principal.oid, true ] ], json_body.dig("data", "principals").map { |l| [ l.dig("principal", "id"), l["default"] ] }

        get api_v1_api_client_path(client.oid)
        assert_response :ok
        assert_not json_body["data"].key?("secret")
        assert_not_includes response.body, secret

        get api_v1_api_clients_path
        assert_not_includes response.body, secret
      end

      test "create works without a principal" do
        post api_v1_api_clients_path, params: { data: { name: "ci", scopes: ApiClient::SCOPES } },
             headers: csrf_headers, as: :json
        assert_response :created
        assert_equal [], json_body.dig("data", "principals")
        assert json_body.dig("data", "secret").present?
      end

      test "a bearer admin key cannot create an API client" do
        assert_no_difference -> { ApiClient.count } do
          post api_v1_api_clients_path, params: { data: { name: "ci", scopes: ApiClient::SCOPES } },
               headers: bearer_headers, as: :json
        end
        assert_response :forbidden
        assert_equal "session_required", json_body.dig("error", "details", "reason")
        assert_no_match(/secret/, response.body)
      end

      test "create rejects an unknown principal" do
        assert_no_difference -> { ApiClient.count } do
          post api_v1_api_clients_path, params: {
            data: { name: "eval-runner", scopes: ApiClient::SCOPES, principal: "prn_missing" }
          }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert_equal [ "No principal matches \"prn_missing\"." ], json_body.dig("error", "details", "base")
      end

      test "create validates name and scopes" do
        assert_no_difference -> { ApiClient.count } do
          post api_v1_api_clients_path, params: { data: { name: "", scopes: [ "" ] } }, headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "name").present?
        assert json_body.dig("error", "details", "scopes").present?
      end

      test "rotate_secret issues a new secret once" do
        client = create_client
        original = client.secret_digest

        post rotate_secret_api_v1_api_client_path(client.oid), headers: csrf_headers, as: :json

        assert_response :ok
        assert_equal "no-store", response.headers["Cache-Control"]
        secret = json_body.dig("data", "secret")
        assert_not_equal original, client.reload.secret_digest
        assert_equal client.secret_digest, ApiClient.digest_secret(secret)
      end

      test "disable and enable toggle access" do
        client = create_client

        post disable_api_v1_api_client_path(client.oid), headers: csrf_headers, as: :json
        assert_response :ok
        assert client.reload.disabled?
        assert_equal true, json_body.dig("data", "disabled")
        disabled_at = client.disabled_at

        post disable_api_v1_api_client_path(client.oid), headers: csrf_headers, as: :json
        assert_equal disabled_at, client.reload.disabled_at

        post enable_api_v1_api_client_path(client.oid), headers: csrf_headers, as: :json
        assert_response :ok
        assert_not client.reload.disabled?
      end

      test "attach, make default, and detach principals" do
        client = create_client

        post attach_principal_api_v1_api_client_path(client.oid), params: { data: { principal: principals(:acme_channel).oid } },
             headers: csrf_headers, as: :json
        assert_response :created
        post attach_principal_api_v1_api_client_path(client.oid), params: { data: { principal: principals(:acme_user_bob).foreign_id } },
             headers: csrf_headers, as: :json
        assert_response :created
        channel, bob = client.api_client_principals.order(:id).to_a
        assert channel.default
        assert_equal bob.oid, json_body.dig("data", "id")

        post make_default_principal_api_v1_api_client_path(client.oid, bob.oid), headers: csrf_headers, as: :json
        assert_response :ok
        assert bob.reload.default
        assert_not channel.reload.default

        delete detach_principal_api_v1_api_client_path(client.oid, channel.oid), headers: csrf_headers
        assert_response :no_content
        assert_equal [ bob ], client.api_client_principals.reload.to_a
      end

      test "attaching an already attached principal keeps one link" do
        client = create_client
        client.attach_principal!(principals(:acme_channel))
        post attach_principal_api_v1_api_client_path(client.oid), params: { data: { principal: principals(:acme_channel).oid } },
             headers: csrf_headers, as: :json
        assert_response :created
        assert_equal 1, client.api_client_principals.count
      end

      test "attach rejects an unknown principal" do
        client = create_client
        post attach_principal_api_v1_api_client_path(client.oid), params: { data: { principal: "nope" } },
             headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert_equal "No principal matches that OID or foreign ID.", json_body.dig("error", "message")
        assert_empty client.api_client_principals
      end

      test "principal links are scoped to their client" do
        client = create_client
        other = create_client(name: "other")
        foreign_link = other.attach_principal!(principals(:acme_channel))

        delete detach_principal_api_v1_api_client_path(client.oid, foreign_link.oid), headers: csrf_headers
        assert_response :not_found
        post make_default_principal_api_v1_api_client_path(client.oid, foreign_link.oid), headers: csrf_headers, as: :json
        assert_response :not_found
        assert ApiClientPrincipal.exists?(foreign_link.id)
      end

      test "update changes settings without touching the secret" do
        client = create_client
        digest = client.secret_digest

        patch api_v1_api_client_path(client.oid), params: {
          data: { name: "evals", scopes: [ "", "sessions:read" ] }
        }, headers: csrf_headers, as: :json

        assert_response :ok
        assert_not json_body["data"].key?("secret")
        client.reload
        assert_equal [ "evals", %w[sessions:read], digest ], [ client.name, client.scopes, client.secret_digest ]
      end

      test "update rejects an empty name" do
        client = create_client
        patch api_v1_api_client_path(client.oid), params: { data: { name: "" } }, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert_equal "eval-runner", client.reload.name
      end

      test "destroy deletes the client and its principal links" do
        client = create_client
        client.attach_principal!(principals(:acme_channel))

        delete api_v1_api_client_path(client.oid), headers: csrf_headers
        assert_response :no_content
        assert_not ApiClient.exists?(client.id)
        assert_equal 0, ApiClientPrincipal.where(api_client_id: client.id).count
      end
    end
  end
end
