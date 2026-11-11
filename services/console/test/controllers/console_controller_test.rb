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

    [ console_roles_url, console_secrets_url,
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

  test "secrets table combines kinds and links names to detail" do
    secret = static_secrets(:acme_prod_api_key)
    get console_secrets_url
    assert_response :ok

    assert_select "table", count: 1
    assert_select "th", text: "Name"
    assert_select "th", text: "Type"
    assert_select "th", text: "Source"
    assert_select "th", text: "Created"
    # Source column shows only the backend label, not the underlying reference.
    assert_select "td span", text: "Env"
    assert_select "body", text: /GITHUB_TOKEN/, count: 0
    assert_select "a[href=?][title=?]", console_secret_path("static", secret.oid), secret.name
    assert_select "time[datetime=?]", secret.created_at.iso8601
  end

  test "secrets table filters by type and searches by name" do
    matching = static_secrets(:acme_prod_api_key)
    excluded_by_name = static_secrets(:acme_staging_api_key)
    excluded_by_type = pg_dsn_secrets(:acme_reporting_pg)

    get console_secrets_url, params: { q: "PROD-API", type: "static" }
    assert_response :ok

    assert_select "select[name=type] option[value=static][selected]"
    assert_select "a[href=?]", console_secret_path("static", matching.oid)
    assert_select "a[href=?]", console_secret_path("static", excluded_by_name.oid), count: 0
    assert_select "a[href=?]", console_secret_path("pg_dsn", excluded_by_type.oid), count: 0
  end

  test "secrets table paginates fifty filtered records per page" do
    now = Time.current
    StaticSecret.insert_all!((1..51).map do |number|
      {
        created_at: now,
        updated_at: now,
        name: "Pagination secret #{number}",
        inject_config: { header: "Authorization" }
      }
    end)

    filters = { q: "Pagination secret", type: "static" }
    get console_secrets_url, params: filters

    assert_response :ok
    assert_select "tbody tr", count: 50
    assert_select "a[href*='page=2'][href*='type=static']", text: "Next"
    assert_select "div", text: /51 secrets.*page 1 of 2/

    get console_secrets_url, params: filters.merge(page: 2)

    assert_response :ok
    assert_select "tbody tr", count: 1
    assert_select "a[href*='page=1']", text: "Previous"
  end

  test "secret detail page offers delete for an editable kind but not for others" do
    static = static_secrets(:acme_prod_api_key)
    get console_secret_url("static", static.oid)
    assert_response :ok
    assert_select "form[action=?][method=?]", console_static_secret_path(static.oid), "post" do
      assert_select "input[name=_method][value=delete]"
    end
    assert_select "button", text: "Delete"
    # A kind without a form (e.g. oauth_token) has no delete route, so no button
    # (the only delete-method form left is the layout's Sign out).
    get console_secret_url("oauth_token", oauth_token_secrets(:acme_gmail_oauth).oid)
    assert_response :ok
    assert_select "button", text: "Delete", count: 0
  end

  test "secret detail page shows the full source reference" do
    secret = oauth_token_secrets(:acme_gmail_oauth)
    get console_secret_url("oauth_token", secret.oid)
    assert_response :ok
    assert_select "h1", text: secret.name
    # The full reference is hidden from the table but shown here.
    assert_select "td", text: "GMAIL_CLIENT_ID"
    assert_select "td", text: "op://eng/gmail/refresh-token"
  end

  test "secret detail page renders editable role grants" do
    secret = static_secrets(:acme_prod_api_key)
    grant = grants(:acme_infra_prod_api_key)
    get console_secret_url("static", secret.oid)
    assert_response :ok
    assert_select "h2", text: "Roles"
    assert_select "form[action=?]", console_secret_grant_role_path("static", secret.oid) do
      assert_select "select[name=role_id][aria-label=?]", "Role to assign"
      assert_select "option[value=?]", roles(:acme_admin_role).oid
      assert_select "option[value=?]", roles(:acme_infra).oid, count: 0
      assert_select "option[value=?]", roles(:globex_infra).oid, count: 1
    end
    assert_select "form[action=?]", console_secret_revoke_role_grant_path("static", secret.oid, grant.oid) do
      assert_select "button[type=submit]", "Unassign"
    end
  end

  test "secret detail page renders for every secret kind" do
    [
      [ "static", static_secrets(:github_token_inject) ],
      [ "gcp_auth", gcp_auth_secrets(:acme_gcs_keyfile) ],   # keyfile source
      [ "gcp_auth", gcp_auth_secrets(:acme_bigquery) ],      # workload_identity provider
      [ "gcp_id_token", gcp_id_token_secrets(:acme_cloud_run) ],
      [ "oauth_token", oauth_token_secrets(:acme_gmail_oauth) ],
      [ "pg_dsn", pg_dsn_secrets(:acme_analytics_pg) ],
      [ "hmac", hmac_secrets(:acme_webhook_hmac) ]
    ].each do |kind, secret|
      get console_secret_url(kind, secret.oid)
      assert_response :ok, "expected #{kind} detail page for #{secret.oid} to render"
    end
  end

  test "gcp_id_token detail page lists audience header and keyfile source" do
    secret = gcp_id_token_secrets(:acme_cloud_run)
    get console_secret_url("gcp_id_token", secret.oid)
    assert_response :ok
    assert_select "dt", text: "Audience"
    assert_select "dd", text: secret.audience
    assert_select "dt", text: "Header"
    assert_select "dd", text: "x-serverless-authorization"
    assert_select "td", text: "CLOUD_RUN_SA_KEYFILE"
  end

  test "secret detail page 404s for an unknown kind or id" do
    get console_secret_url("bogus", "ssr_whatever")
    assert_response :not_found
    get console_secret_url("static", "ssr_missing")
    assert_response :not_found
  end

  test "credentials table combines id, shows status, and links to detail" do
    credential = broker_credentials(:acme_managed_gmail)
    get console_credentials_url
    assert_response :ok
    assert_select "a[href=?][title=?]", console_credential_path(credential.oid), credential.foreign_id
    assert_select "div", text: credential.oid
    assert_select "span", text: credential.status
  end

  test "secrets table badges an OAuth-flow-managed static secret" do
    secret = static_secrets(:acme_managed_gmail_secret)
    get console_secrets_url
    assert_response :ok
    assert_select "span", text: "managed"
    # Only the wrapping secret is badged, not ordinary static secrets.
    assert_select "span", text: "managed", count: StaticSecret.where.not(broker_credential_id: nil).count
  end

  test "managed secret detail page links to the credential it wraps" do
    secret = static_secrets(:acme_managed_gmail_secret)
    cred = secret.broker_credential
    get console_secret_url("static", secret.oid)
    assert_response :ok
    assert_match "Managed secret", response.body
    assert_select "a[href=?]", console_credential_path(cred.oid)
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
