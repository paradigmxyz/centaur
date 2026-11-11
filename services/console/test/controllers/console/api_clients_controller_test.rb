require "test_helper"

module Console
  class ApiClientsControllerTest < ActionDispatch::IntegrationTest
    setup do
      ENV["CENTAUR_CONSOLE_API_CLIENTS_ENABLED"] = "true"
      post login_url, params: { email: users(:acme_admin).email, password: "password123456" }
    end

    teardown do
      ENV.delete("CENTAUR_CONSOLE_API_CLIENTS_ENABLED")
    end

    def create_client(name: "eval-runner")
      ApiClient.create!(name: name, scopes: ApiClient::SCOPES, created_by: users(:acme_admin))
    end

    test "API clients are hidden and unreachable while the feature is off" do
      client = create_client
      ENV.delete("CENTAUR_CONSOLE_API_CLIENTS_ENABLED")

      get console_credentials_url
      assert_select "a[href=?]", console_api_clients_path, count: 0

      get console_api_clients_url
      assert_response :not_found
      post rotate_secret_console_api_client_url(client)
      assert_response :not_found
      assert_no_difference -> { ApiClient.count } do
        post console_api_clients_url, params: { api_client: { name: "x", scopes: ApiClient::SCOPES } }
      end
      assert_response :not_found
    end

    test "the API Clients tab shows for admins while the feature is on" do
      get console_credentials_url
      assert_select "a[href=?]", console_api_clients_path, text: "API Clients"
    end

    test "non-admins cannot manage API clients" do
      delete logout_url
      post login_url, params: { email: users(:member_user).email, password: "password123456" }

      get console_api_clients_url
      assert_response :redirect

      assert_no_difference -> { ApiClient.count } do
        post console_api_clients_url, params: { api_client: { name: "x", scopes: ApiClient::SCOPES } }
      end
    end

    test "create shows the secret once with its default principal attached" do
      principal = principals(:acme_user_alice)

      post console_api_clients_url, params: {
        api_client: { name: "eval-runner", scopes: [ "", "sessions:read" ], principal: principal.foreign_id }
      }

      assert_response :ok
      client = ApiClient.order(:id).last
      assert_equal %w[sessions:read], client.scopes
      assert_equal principal, client.default_principal_link.principal
      assert_equal "no-store", response.headers["Cache-Control"]
      secret = css_select("#api_client_secret").first["value"]
      assert_equal client.secret_digest, ApiClient.digest_secret(secret)

      get console_api_client_url(client)
      assert_response :ok
      assert_select "#api_client_secret", count: 0
      assert_not_includes response.body, secret
    end

    test "create rejects an unknown principal" do
      assert_no_difference -> { ApiClient.count } do
        post console_api_clients_url, params: {
          api_client: { name: "eval-runner", scopes: ApiClient::SCOPES, principal: "prn_missing" }
        }
      end
      assert_response :unprocessable_entity
    end

    test "rotate_secret asks for confirmation and issues a new secret" do
      client = create_client
      original = client.secret_digest

      get console_api_client_url(client)
      assert_select "form[action=?][onsubmit*=confirm]", rotate_secret_console_api_client_path(client)

      post rotate_secret_console_api_client_url(client)

      assert_response :ok
      secret = css_select("#api_client_secret").first["value"]
      assert_not_equal original, client.reload.secret_digest
      assert_equal client.secret_digest, ApiClient.digest_secret(secret)
    end

    test "disable and enable toggle access" do
      client = create_client

      post disable_console_api_client_url(client)
      assert client.reload.disabled?

      post enable_console_api_client_url(client)
      assert_not client.reload.disabled?
    end

    test "attach, make default, and detach principals" do
      client = create_client

      post attach_principal_console_api_client_url(client), params: { principal: principals(:acme_channel).oid }
      post attach_principal_console_api_client_url(client), params: { principal: principals(:acme_user_bob).foreign_id }
      channel, bob = client.api_client_principals.order(:id).to_a
      assert channel.default

      post make_default_principal_console_api_client_url(client, bob.oid)
      assert bob.reload.default
      assert_not channel.reload.default

      delete detach_principal_console_api_client_url(client, channel.oid)
      assert_equal [ bob ], client.api_client_principals.reload.to_a
    end

    test "update changes settings without touching the secret" do
      client = create_client
      digest = client.secret_digest

      patch console_api_client_url(client), params: {
        api_client: { name: "evals", scopes: [ "", "sessions:read" ] }
      }

      assert_redirected_to console_api_client_path(client)
      client.reload
      assert_equal [ "evals", %w[sessions:read], digest ], [ client.name, client.scopes, client.secret_digest ]
    end
  end
end
