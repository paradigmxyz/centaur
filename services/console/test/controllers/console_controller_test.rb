require "test_helper"

class ConsoleControllerTest < ActionDispatch::IntegrationTest
  setup do
    @operator = users(:acme_admin)
    post login_url, params: { email: @operator.email, password: "password123456" }
  end

  test "redirects to login when not signed in" do
    delete logout_url
    get console_roles_url
    assert_redirected_to login_path
  end

  test "an active non-admin is redirected away from every Control page" do
    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    [ console_roles_url,
      console_credentials_url, console_oauth_apps_url ].each do |url|
      get url
      assert_redirected_to console_integrations_path
      assert_nil flash[:alert]
    end
  end

  test "a non-admin cannot mutate through the Control form controllers" do
    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    assert_no_difference -> { Role.count } do
      post console_roles_url, params: { role: { foreign_id: "sneaky" } }
    end
    assert_redirected_to console_integrations_path
  end

  test "credentials table combines id, shows status, and links to detail" do
    credential = broker_credentials(:acme_managed_gmail)
    get console_credentials_url
    assert_response :ok
    assert_select "a[href=?][title=?]", console_credential_path(credential.oid), credential.foreign_id
    assert_select "div", text: credential.oid
    assert_select "span", text: credential.status
  end

  test "credential detail page links to the static secret that makes it grantable" do
    cred = broker_credentials(:acme_managed_gmail)
    secret = static_secrets(:acme_managed_gmail_secret)
    get console_credential_url(cred.oid)
    assert_response :ok
    assert_select "h2", text: "Grantable As"
    assert_select "a[href=?]", console_secret_path("static", secret.oid)
  end

  test "credential detail page shows refresh and client data" do
    credential = broker_credentials(:acme_managed_gmail)
    get console_credential_url(credential.oid)
    assert_response :ok
    assert_select "h1", text: credential.name
    # The next-refresh data lives here now (removed from the index table).
    assert_select "dt", text: "Next attempt"
    assert_select "dd", text: credential.client_id
    assert_select "dd", text: credential.token_endpoint
    # Token material is never rendered.
    assert_select "body", text: /access[_ ]token/i, count: 0
  end

  test "credential detail page 404s for an unknown id" do
    get console_credential_url("bcr_missing")
    assert_response :not_found
  end

  test "oauth apps table lists apps and links to detail" do
    app = oauth_apps(:acme_google)
    get console_oauth_apps_url
    assert_response :ok
    assert_select "a[href=?]", console_oauth_app_path(app.oid)
    assert_select "span", text: app.provider
  end

  test "oauth app detail page shows config, the redirect URI, and a start URL" do
    app = oauth_apps(:acme_google)
    app.update!(client_secret: "shh")
    get console_oauth_app_url(app.oid)
    assert_response :ok
    assert_select "h1", text: app.slug
    assert_select "dd", text: app.client_id
    assert_select "dd", text: "set" # client secret presence, never the value
    assert_includes response.body, "/oauth/google/callback"
    assert_includes response.body, "/oauth/google/start"
  end

  test "oauth app detail page 404s for an unknown id" do
    get console_oauth_app_url("oap_missing")
    assert_response :not_found
  end

  test "credential detail page shows the provider identity for a flow-minted credential" do
    app = oauth_apps(:acme_google)
    cred = BrokerCredential.create!(foreign_id: "minted-1",
                                    token_endpoint: "https://oauth2.googleapis.com/token",
                                    oauth_app: app, provider_subject: "sub-9",
                                    provider_email: "person@example.com", external_user_key: "user-9")
    get console_credential_url(cred.oid)
    assert_response :ok
    assert_select "dd", text: "person@example.com"
    assert_select "a[href=?]", console_oauth_app_path(app.oid)
  end

  test "header shows the signed-in operator and a sign-out control" do
    get console_roles_url
    assert_response :ok
    assert_select "span", text: @operator.email
    assert_select "form[action=?][method=?]", logout_path, "post" do
      assert_select "input[name=_method][value=delete]", count: 1
      assert_select "button", text: "Sign out"
    end
  end
end
