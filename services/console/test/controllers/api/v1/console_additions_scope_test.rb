require "test_helper"

module Api
  module V1
    # The console API serves several bearer resources to the SPA through thin
    # Api::V1::Console subclasses that add fields, filters, and write
    # semantics. Those additions exist only under /api/v1/console; the bearer
    # API keeps its own contract.
    class ConsoleAdditionsScopeTest < ActionDispatch::IntegrationTest
      setup do
        sign_in_console(users(:acme_admin))
      end

      test "grant payloads add the grantable summary only in the console API" do
        grant = grants(:acme_infra_prod_api_key)

        get api_v1_grant_path(grant.oid), headers: bearer_headers
        assert_response :ok
        assert_not json_body["data"].key?("grantable")
        get api_v1_role_grants_path(grant.role.oid), headers: bearer_headers
        assert_not json_body["data"].first.key?("grantable")

        get api_v1_console_grant_path(grant.oid)
        assert_equal "static", json_body.dig("data", "grantable", "kind")
        get api_v1_console_role_grants_path(grant.role.oid)
        assert_equal "static", json_body.dig("data", 0, "grantable", "kind")
      end

      test "broker credential presence flags and references are console-only" do
        credential = broker_credentials(:acme_managed_gmail)
        console_keys = %w[has_client_secret has_refresh_token has_username has_password has_api_key
                          effective_client_id oauth_app static_secret]

        get api_v1_broker_credential_path(credential.oid), headers: bearer_headers
        assert_response :ok
        assert_empty console_keys & json_body["data"].keys

        get api_v1_console_broker_credential_path(credential.oid)
        assert_response :ok
        assert_equal console_keys.sort, (console_keys & json_body["data"].keys).sort
      end

      test "OAuth app URL and count fields are console-only" do
        app = oauth_apps(:acme_google)
        console_keys = %w[client_secret_set redirect_uri start_url credentials_count]

        get api_v1_oauth_app_path(app.oid), headers: bearer_headers
        assert_empty console_keys & json_body["data"].keys
        get api_v1_oauth_apps_path, headers: bearer_headers
        assert_empty console_keys & json_body["data"].first.keys

        get api_v1_console_oauth_app_path(app.oid)
        assert_equal console_keys.sort, (console_keys & json_body["data"].keys).sort
      end

      test "principal identity fields are console-only" do
        principal = principals(:acme_user_alice)

        get api_v1_principal_path(principal.oid), headers: bearer_headers
        assert_empty %w[kind slack_user_id roles] & json_body["data"].keys

        get api_v1_console_principal_path(principal.oid)
        assert_equal principal.kind, json_body.dig("data", "kind")
      end

      test "the roles q and exclude_principal filters are console-only" do
        get api_v1_roles_path(q: "no-such-role-anywhere", exclude_principal: "prn_missing"), headers: bearer_headers
        assert_response :ok
        assert_equal Role.count, json_body.dig("meta", "total")

        get api_v1_console_roles_path(q: "no-such-role-anywhere")
        assert_response :ok
        assert_equal 0, json_body.dig("meta", "total")
      end

      test "a bearer PATCH that omits an inline secret value does not keep the stored one" do
        secret = StaticSecret.create!(foreign_id: "inline-scope", name: "inline-scope", created_by: users(:acme_admin),
                                      inject_config: { "header" => "Authorization" })
        SecretSource.create!(static_secret: secret, source_type: "control_plane", secret: "s3cr3t-value")
        body = { data: { name: "renamed", inject_config: { header: "Authorization" },
                         source: { source_type: "control_plane" } } }

        patch api_v1_static_secret_path(secret.oid), params: body, headers: bearer_headers, as: :json
        assert_response :unprocessable_entity
        assert_equal "inline-scope", secret.reload.name

        patch api_v1_console_static_secret_path(secret.oid), params: body, headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal "renamed", secret.reload.name
        assert_equal "s3cr3t-value", secret.source.secret
        assert_no_match(/s3cr3t-value/, response.body)
      end

      test "nested 422 path keys are console-only" do
        body = { data: { foreign_id: "bad-rule", name: "bad-rule", inject_config: { header: "Authorization" },
                         source: { source_type: "env", config: { var: "X" } },
                         rules: [ { cidr: "not-a-cidr" } ] } }

        post api_v1_static_secrets_path, params: body, headers: bearer_headers, as: :json
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details").keys.none? { |key| key.start_with?("rules[") }

        post api_v1_console_static_secrets_path, params: body, headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details").keys.any? { |key| key.start_with?("rules[0].") }
        assert_not StaticSecret.exists?(foreign_id: "bad-rule")
      end

      test "a null header value keeps a stored broker credential header only in the console API" do
        credential = broker_credentials(:acme_managed_gmail)
        credential.update!(token_endpoint_headers: { "X-Api-Key" => "stored-value" })
        body = { data: { token_endpoint_headers: { "X-Api-Key" => nil } } }

        patch api_v1_broker_credential_path(credential.oid), params: body, headers: bearer_headers, as: :json
        assert_response :unprocessable_entity
        assert_equal({ "X-Api-Key" => "stored-value" }, credential.reload.token_endpoint_headers)

        patch api_v1_console_broker_credential_path(credential.oid), params: body, headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal({ "X-Api-Key" => "stored-value" }, credential.reload.token_endpoint_headers)
      end
    end
  end
end
