require "test_helper"

class ConsoleControllerTest < ActionDispatch::IntegrationTest
  setup do
    @operator = users(:acme_admin)
    post login_url, params: { email: @operator.email, password: "password123456" }
  end

  test "redirects to login when not signed in" do
    delete logout_url
    get console_principals_url
    assert_redirected_to login_path
  end

  test "an active non-admin is redirected away from every Control page" do
    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    [ root_url, console_principals_url, console_roles_url, console_secrets_url,
      console_credentials_url, console_oauth_apps_url ].each do |url|
      get url
      assert_redirected_to console_threads_path
      assert_equal "That page is restricted to admins.", flash[:alert]
    end
  end

  test "a non-admin cannot mutate through the Control form controllers" do
    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    assert_no_difference -> { Role.count } do
      post console_roles_url, params: { role: { foreign_id: "sneaky", namespace: "default" } }
    end
    assert_redirected_to console_threads_path
  end

  test "secrets table shows backend labels (not refs) and links to detail" do
    secret = static_secrets(:acme_prod_api_key)
    get console_secrets_url
    assert_response :ok
    # Source column shows only the backend label, not the underlying reference.
    assert_select "td span", text: "Env"
    assert_select "body", text: /GITHUB_TOKEN/, count: 0
    # The foreign_id links to the detail page (full value as a hover tooltip),
    # with the opaque oid and namespace shown beneath it.
    assert_select "a[href=?][title=?]", console_secret_path("static", secret.oid), secret.foreign_id
    assert_select "div", text: /#{Regexp.escape(secret.oid)}.*#{Regexp.escape(secret.namespace)}/
    # The name is plain text (not a link) with the full value as a tooltip.
    assert_select "span[title=?]", secret.name
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
      assert_select "option[value=?]", roles(:globex_infra).oid, count: 0
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

  test "pg_dsn detail page lists configured session settings" do
    secret = pg_dsn_secrets(:acme_analytics_pg)
    secret.update!(settings: [ { "name" => "app.tenant", "value" => "centaur" } ])
    get console_secret_url("pg_dsn", secret.oid)
    assert_response :ok
    assert_select "dt", text: "Session settings"
    assert_select "dd", text: "app.tenant = centaur"
  end

  test "secret detail page 404s for an unknown kind or id" do
    get console_secret_url("bogus", "ssr_whatever")
    assert_response :not_found
    get console_secret_url("static", "ssr_missing")
    assert_response :not_found
  end

  test "principals table combines id with foreign_id over the oid" do
    principal = principals(:acme_channel)
    get console_principals_url
    assert_response :ok
    # foreign_id is the primary line (with a hover tooltip); the oid and
    # namespace sit beneath it.
    assert_select "div[title=?]", principal.foreign_id, text: principal.foreign_id
    assert_select "div", text: /#{Regexp.escape(principal.oid)}.*#{Regexp.escape(principal.namespace)}/
  end

  test "principals table links to add principal" do
    get console_principals_url
    assert_response :ok
    assert_select "a[href=?]", console_new_principal_path, text: "Add Principal"
  end

  test "principal detail page offers delete" do
    principal = principals(:acme_channel)
    get console_principal_url(principal.oid)
    assert_response :ok
    assert_select "form[action=?][method=?]", console_delete_principal_path(principal.oid), "post" do
      assert_select "input[name=_method][value=delete]"
      assert_select "button[type=submit]", "Delete"
    end
  end

  test "credentials table combines id, shows status, and links to detail" do
    credential = broker_credentials(:acme_managed_gmail)
    get console_credentials_url
    assert_response :ok
    assert_select "a[href=?][title=?]", console_credential_path(credential.oid), credential.foreign_id
    assert_select "div", text: /#{Regexp.escape(credential.oid)}.*#{Regexp.escape(credential.namespace)}/
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
    cred = BrokerCredential.create!(namespace: "acme", foreign_id: "minted-1",
                                    token_endpoint: "https://oauth2.googleapis.com/token",
                                    oauth_app: app, provider_subject: "sub-9",
                                    provider_email: "person@example.com", external_user_key: "user-9")
    get console_credential_url(cred.oid)
    assert_response :ok
    assert_select "dd", text: "person@example.com"
    assert_select "a[href=?]", console_oauth_app_path(app.oid)
  end

  test "principal detail page renders the role and direct-grant management forms" do
    principal = principals(:acme_channel)
    get console_principal_url(principal.oid)
    assert_response :ok
    assert_select "h2", text: "Roles"
    assert_select "form[action=?]", console_principal_assign_role_path(principal.oid) do
      assert_select "select[name=role_id][aria-label=?]", "Role to assign"
      assert_select "option[value=?]", roles(:acme_admin_role).oid
      assert_select "option[value=?]", roles(:globex_infra).oid, count: 0
    end
    assert_select "form[action=?]", console_principal_unassign_role_path(principal.oid, roles(:acme_infra).oid) do
      assert_select "button[type=submit]", "Unassign"
    end
    assert_select "h2", text: "Direct Grants"
    assert_select "select[name=grantable] optgroup"
    # Each direct grant exposes a revoke form.
    assert_select "form[action=?]", console_principal_revoke_grant_path(principal.oid, grants(:acme_channel_github_token).oid)
    # The direct grant's id links to the secret's detail page.
    assert_select "a[href=?]", console_secret_path("static", static_secrets(:github_token_inject).oid)
  end

  test "effective grants table sources each secret as direct or via a role" do
    principal = principals(:acme_channel) # direct grants + acme_prod_api_key via the acme_infra role
    get console_principal_url(principal.oid)
    assert_response :ok
    # The Source column exists.
    assert_select "section table th", text: "Source"
    # A directly granted secret is tagged "direct".
    assert_select "td", text: "direct"
    # The role-inherited secret names the role it comes through, and its id links
    # to the secret (it appears only in the effective table, not the direct one).
    assert_select "td span", text: "via #{roles(:acme_infra).name}"
    assert_select "a[href=?]", console_secret_path("static", static_secrets(:acme_prod_api_key).oid)
  end

  test "grant dropdown omits secrets already granted directly but keeps the rest" do
    principal = principals(:acme_channel) # github_token_inject is granted directly
    get console_principal_url(principal.oid)
    assert_response :ok
    granted = static_secrets(:github_token_inject)
    ungranted = static_secrets(:acme_staging_api_key)
    assert_select "select[name=grantable] option[value=?]", "static:#{granted.oid}", count: 0
    assert_select "select[name=grantable] option[value=?]", "static:#{ungranted.oid}"
    # A kind with no direct grant on this principal still lists all its secrets.
    gcp = gcp_auth_secrets(:acme_bigquery)
    assert_select "select[name=grantable] option[value=?]", "gcp_auth:#{gcp.oid}"
    gcp_id = gcp_id_token_secrets(:acme_cloud_run)
    assert_select "select[name=grantable] option[value=?]", "gcp_id_token:#{gcp_id.oid}"
  end

  test "header shows the signed-in operator and a sign-out control" do
    get console_principals_url
    assert_response :ok
    assert_select "span", text: @operator.email
    assert_select "form[action=?][method=?]", logout_path, "post" do
      assert_select "input[name=_method][value=delete]", count: 1
      assert_select "button", text: "Sign out"
    end
  end
end
