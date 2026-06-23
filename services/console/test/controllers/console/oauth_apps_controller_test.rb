require "test_helper"

module Console
  # Covers the OAuth app create/edit form controller: what gets built and
  # persisted (identity, provider, OAuth client, flow config, labels, the write-
  # only client_secret), redirects, that a blank client_secret leaves the stored
  # value in place, and that invalid input is rejected (422) without writing.
  class OauthAppsControllerTest < ActionDispatch::IntegrationTest
    setup do
      @operator = users(:acme_admin)
      post login_url, params: { email: @operator.email, password: "password123456" }
    end

    test "redirects to login when not signed in" do
      delete logout_url
      get new_console_oauth_app_form_url
      assert_redirected_to login_path
    end

    test "GET new and edit render without error" do
      get new_console_oauth_app_form_url
      assert_response :ok
      get edit_console_oauth_app_form_url(oauth_apps(:acme_google).oid)
      assert_response :ok
    end

    test "POST create builds an app with all fields" do
      assert_difference -> { OauthApp.count } => 1 do
        post console_oauth_app_forms_url, params: {
          oauth_app: {
            slug: "new-google", description: "New Google integration",
            provider: "google", client_id: "cid", client_secret: "shh",
            credential_namespace: "acme", enabled: "1",
            allowed_scopes: "https://www.googleapis.com/auth/gmail.readonly\nhttps://www.googleapis.com/auth/calendar.readonly\n"
          },
          labels: { "0" => { key: "team", value: "comms" } }
        }
      end

      app = OauthApp.find_by!(slug: "new-google")
      assert_redirected_to console_oauth_app_path(app.oid)
      assert_equal "google", app.provider
      assert_equal "cid", app.client_id
      assert_equal "shh", app.client_secret
      assert_equal "New Google integration", app.description
      assert_equal %w[https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/calendar.readonly], app.allowed_scopes
      assert_equal({ "team" => "comms" }, app.labels)
      assert app.enabled?
      assert_equal @operator, app.created_by
    end

    test "POST create with an invalid provider is rejected without writing" do
      assert_no_difference -> { OauthApp.count } do
        post console_oauth_app_forms_url, params: {
          oauth_app: {
            provider: "unsupported", slug: "bad-provider", client_id: "cid", client_secret: "shh",
            allowed_scopes: "a"
          }
        }
      end
      assert_response :unprocessable_entity
    end

    test "POST create with no allowed scopes is rejected" do
      assert_no_difference -> { OauthApp.count } do
        post console_oauth_app_forms_url, params: {
          oauth_app: {
            provider: "google", slug: "no-scopes", client_id: "cid", client_secret: "shh",
            allowed_scopes: ""
          }
        }
      end
      assert_response :unprocessable_entity
    end

    test "PATCH update changes attributes and replaces lists" do
      app = oauth_apps(:acme_google)
      app.update!(client_secret: "original")
      patch console_oauth_app_form_url(app.oid), params: {
        oauth_app: {
          slug: app.slug, description: "Renamed",
          provider: "google", client_id: "new-cid", credential_namespace: app.credential_namespace,
          enabled: "0",
          allowed_scopes: "https://www.googleapis.com/auth/gmail.readonly"
        }
      }
      assert_redirected_to console_oauth_app_path(app.oid)
      app.reload
      assert_equal "Renamed", app.description
      assert_equal "new-cid", app.client_id
      refute app.enabled?
      assert_equal %w[https://www.googleapis.com/auth/gmail.readonly], app.allowed_scopes
    end

    test "PATCH update with a blank client_secret keeps the stored value" do
      app = oauth_apps(:acme_google)
      app.update!(client_secret: "original-secret")
      patch console_oauth_app_form_url(app.oid), params: {
        oauth_app: {
          slug: app.slug,
          provider: "google", client_id: app.client_id, client_secret: "",
          credential_namespace: app.credential_namespace, enabled: "1",
          allowed_scopes: Array(app.allowed_scopes).join("\n")
        }
      }
      assert_redirected_to console_oauth_app_path(app.oid)
      assert_equal "original-secret", app.reload.client_secret
    end
  end
end
