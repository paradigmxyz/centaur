require "test_helper"

module Console
  # Covers the broker credential create/edit form controller: what gets built and
  # persisted (identity, OAuth config, scopes, headers, refresh policy, write-
  # only initial values), redirects, that blank write-only fields leave the
  # stored values in place, and that invalid input is rejected (422) without writing.
  class BrokerCredentialsControllerTest < ActionDispatch::IntegrationTest
    setup do
      @operator = users(:acme_admin)
      post login_url, params: { email: @operator.email, password: "password123456" }
    end

    # --- routing / gating -------------------------------------------------

    test "redirects to login when not signed in" do
      delete logout_url
      get new_console_broker_credential_url
      assert_redirected_to login_path
    end

    test "GET new and edit render without error" do
      get new_console_broker_credential_url
      assert_response :ok
      get edit_console_broker_credential_url(broker_credentials(:acme_managed_gmail).oid)
      assert_response :ok
    end

    # --- create -----------------------------------------------------------

    test "POST create builds a credential with config, scopes, headers, and a seed" do
      assert_difference -> { BrokerCredential.count } => 1 do
        post console_broker_credentials_url, params: {
          credential: {
            namespace: "acme", foreign_id: "new-managed", name: "new-managed",
            token_endpoint: "https://idp.example/token", client_id: "cid",
            client_secret: "shhh", scopes: "scope.a\nscope.b\n",
            refresh_token: "seed-token",
            early_refresh_fraction: "0.5", early_refresh_slack_seconds: "120",
            max_refresh_interval_seconds: "3600", refresh_timeout_seconds: "10"
          },
          headers: { "0" => { key: "Authorization", value: "Basic abc" } },
          labels: { "0" => { key: "team", value: "comms" } }
        }
      end

      cred = BrokerCredential.find_by!(namespace: "acme", foreign_id: "new-managed")
      assert_redirected_to console_credential_path(cred.oid)
      assert_equal %w[scope.a scope.b], cred.scopes
      assert_equal({ "Authorization" => "Basic abc" }, cred.token_endpoint_headers)
      assert_equal({ "team" => "comms" }, cred.labels)
      assert_equal "shhh", cred.client_secret
      assert_equal "seed-token", cred.refresh_token
      assert_equal 0.5, cred.early_refresh_fraction
      assert_equal 120, cred.early_refresh_slack_seconds
      assert_equal @operator, cred.created_by
    end

    test "POST create builds a password grant credential with write-only initial values" do
      assert_difference -> { BrokerCredential.count } => 1 do
        post console_broker_credentials_url, params: {
          credential: {
            namespace: "acme", foreign_id: "alphasense", name: "AlphaSense",
            grant: "password", token_endpoint: "https://api.alpha-sense.com/auth",
            client_id: "alpha-client", client_secret: "alpha-secret",
            username: "alpha-user", password: "alpha-password",
            early_refresh_fraction: "0.5", early_refresh_slack_seconds: "120",
            max_refresh_interval_seconds: "3600", refresh_timeout_seconds: "10"
          },
          headers: { "0" => { key: "x-api-key", value: "alpha-key" } }
        }
      end

      cred = BrokerCredential.find_by!(namespace: "acme", foreign_id: "alphasense")
      assert_redirected_to console_credential_path(cred.oid)
      assert_equal "password", cred.grant
      assert_equal "alpha-user", cred.username
      assert_equal "alpha-password", cred.password
      assert_equal "alpha-secret", cred.client_secret
      assert_equal({ "x-api-key" => "alpha-key" }, cred.token_endpoint_headers)
    end

    test "POST create builds a preqin credential with write-only API key" do
      assert_difference -> { BrokerCredential.count } => 1 do
        post console_broker_credentials_url, params: {
          credential: {
            namespace: "acme", foreign_id: "preqin", name: "Preqin",
            grant: "preqin",
            preqin_username: "preqin-user", api_key: "preqin-api-key",
            early_refresh_fraction: "0.5", early_refresh_slack_seconds: "120",
            max_refresh_interval_seconds: "3600", refresh_timeout_seconds: "10"
          }
        }
      end

      cred = BrokerCredential.find_by!(namespace: "acme", foreign_id: "preqin")
      assert_redirected_to console_credential_path(cred.oid)
      assert_equal "preqin", cred.grant
      assert_equal BrokerCredential::PREQIN_TOKEN_ENDPOINT, cred.token_endpoint
      assert_nil cred.client_id
      assert_equal "preqin-user", cred.username
      assert_equal "preqin-api-key", cred.api_key
    end

    test "POST create without a token endpoint or client id is rejected without writing" do
      assert_no_difference "BrokerCredential.count" do
        post console_broker_credentials_url, params: {
          credential: { namespace: "acme", foreign_id: "broken", client_id: "" }
        }
      end
      assert_response :unprocessable_entity
    end

    # --- update -----------------------------------------------------------

    test "PATCH update changes attributes and replaces scopes and headers" do
      cred = broker_credentials(:acme_managed_gmail)
      patch console_broker_credential_url(cred.oid), params: {
        credential: {
          namespace: cred.namespace, foreign_id: cred.foreign_id, name: "renamed",
          token_endpoint: cred.token_endpoint, client_id: "new-cid",
          scopes: "only.scope",
          early_refresh_fraction: cred.early_refresh_fraction,
          early_refresh_slack_seconds: cred.early_refresh_slack_seconds,
          max_refresh_interval_seconds: cred.max_refresh_interval_seconds,
          refresh_timeout_seconds: cred.refresh_timeout_seconds
        },
        headers: { "0" => { key: "X-Tenant", value: "acme" } }
      }
      assert_redirected_to console_credential_path(cred.oid)
      cred.reload
      assert_equal "renamed", cred.name
      assert_equal "new-cid", cred.client_id
      assert_equal [ "only.scope" ], cred.scopes
      assert_equal({ "X-Tenant" => "acme" }, cred.token_endpoint_headers)
    end

    test "PATCH update with blank client_secret and refresh_token leaves them in place" do
      cred = broker_credentials(:acme_managed_gmail)
      cred.update!(client_secret: "original-secret", refresh_token: "original-token", dead: true, dead_reason: "stale")

      patch console_broker_credential_url(cred.oid), params: {
        credential: {
          namespace: cred.namespace, foreign_id: cred.foreign_id,
          token_endpoint: cred.token_endpoint, client_id: cred.client_id,
          client_secret: "", refresh_token: "",
          early_refresh_fraction: cred.early_refresh_fraction,
          early_refresh_slack_seconds: cred.early_refresh_slack_seconds,
          max_refresh_interval_seconds: cred.max_refresh_interval_seconds,
          refresh_timeout_seconds: cred.refresh_timeout_seconds
        }
      }
      assert_redirected_to console_credential_path(cred.oid)
      cred.reload
      assert_equal "original-secret", cred.client_secret
      assert_equal "original-token", cred.refresh_token
      # A blank seed must not reschedule: dead state is untouched.
      assert cred.dead?
    end

    test "PATCH update with blank password grant fields leaves them in place" do
      cred = BrokerCredential.create!(namespace: "acme", foreign_id: "password-console",
                                      grant: "password", token_endpoint: "https://idp.example/token",
                                      client_id: "cid", client_secret: "secret",
                                      username: "user", password: "pass", created_by: @operator)

      patch console_broker_credential_url(cred.oid), params: {
        credential: {
          namespace: cred.namespace, foreign_id: cred.foreign_id,
          grant: "password", token_endpoint: cred.token_endpoint, client_id: cred.client_id,
          client_secret: "", username: "", password: "",
          early_refresh_fraction: cred.early_refresh_fraction,
          early_refresh_slack_seconds: cred.early_refresh_slack_seconds,
          max_refresh_interval_seconds: cred.max_refresh_interval_seconds,
          refresh_timeout_seconds: cred.refresh_timeout_seconds
        }
      }
      assert_redirected_to console_credential_path(cred.oid)
      cred.reload
      assert_equal "secret", cred.client_secret
      assert_equal "user", cred.username
      assert_equal "pass", cred.password
    end

    test "PATCH update with blank preqin fields leaves them in place" do
      cred = BrokerCredential.create!(namespace: "acme", foreign_id: "preqin-console",
                                      grant: "preqin", username: "user", api_key: "api-key",
                                      created_by: @operator)

      patch console_broker_credential_url(cred.oid), params: {
        credential: {
          namespace: cred.namespace, foreign_id: cred.foreign_id,
          grant: "preqin", token_endpoint: cred.token_endpoint,
          preqin_username: "", api_key: "",
          early_refresh_fraction: cred.early_refresh_fraction,
          early_refresh_slack_seconds: cred.early_refresh_slack_seconds,
          max_refresh_interval_seconds: cred.max_refresh_interval_seconds,
          refresh_timeout_seconds: cred.refresh_timeout_seconds
        }
      }
      assert_redirected_to console_credential_path(cred.oid)
      cred.reload
      assert_equal "user", cred.username
      assert_equal "api-key", cred.api_key
    end

    test "PATCH preqin uses preqin username over hidden password username" do
      cred = BrokerCredential.create!(namespace: "acme", foreign_id: "preqin-username",
                                      grant: "preqin", username: "old-user", api_key: "api-key",
                                      created_by: @operator)

      patch console_broker_credential_url(cred.oid), params: {
        credential: {
          namespace: cred.namespace, foreign_id: cred.foreign_id,
          grant: "preqin", token_endpoint: cred.token_endpoint,
          username: "password-panel-user", preqin_username: "preqin-panel-user", api_key: "",
          early_refresh_fraction: cred.early_refresh_fraction,
          early_refresh_slack_seconds: cred.early_refresh_slack_seconds,
          max_refresh_interval_seconds: cred.max_refresh_interval_seconds,
          refresh_timeout_seconds: cred.refresh_timeout_seconds
        }
      }
      assert_redirected_to console_credential_path(cred.oid)
      cred.reload
      assert_equal "preqin-panel-user", cred.username
      assert_equal "api-key", cred.api_key
    end

    test "PATCH update with a fresh refresh_token reschedules the credential" do
      cred = broker_credentials(:acme_managed_gmail)
      cred.update!(refresh_token: "old", dead: true, dead_reason: "stale", failure_count: 3)

      patch console_broker_credential_url(cred.oid), params: {
        credential: {
          namespace: cred.namespace, foreign_id: cred.foreign_id,
          token_endpoint: cred.token_endpoint, client_id: cred.client_id,
          refresh_token: "fresh-seed",
          early_refresh_fraction: cred.early_refresh_fraction,
          early_refresh_slack_seconds: cred.early_refresh_slack_seconds,
          max_refresh_interval_seconds: cred.max_refresh_interval_seconds,
          refresh_timeout_seconds: cred.refresh_timeout_seconds
        }
      }
      cred.reload
      assert_equal "fresh-seed", cred.refresh_token
      assert_not cred.dead?
      assert_nil cred.dead_reason
      assert_equal 0, cred.failure_count
    end

    # --- destroy ----------------------------------------------------------

    test "DELETE destroy removes an unreferenced credential" do
      cred = broker_credentials(:globex_managed_api)
      assert_difference -> { BrokerCredential.count } => -1 do
        delete console_broker_credential_url(cred.oid)
      end
      assert_redirected_to console_credentials_path
      assert_equal "Credential deleted.", flash[:notice]
    end

    test "DELETE destroy is blocked while a token_broker source references it" do
      cred = broker_credentials(:globex_managed_api)
      SecretSource.create!(source_type: "token_broker",
                           config: { "credential_id" => cred.foreign_id, "credential_namespace" => cred.namespace })
      assert_no_difference -> { BrokerCredential.count } do
        delete console_broker_credential_url(cred.oid)
      end
      assert_redirected_to console_credential_path(cred.oid)
      assert_match "referenced by", flash[:alert]
    end
  end
end
