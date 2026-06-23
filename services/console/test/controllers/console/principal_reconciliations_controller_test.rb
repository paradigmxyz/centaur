require "test_helper"

module Console
  class PrincipalReconciliationsControllerTest < ActionDispatch::IntegrationTest
    setup do
      @operator = users(:acme_admin)
      post login_url, params: { email: @operator.email, password: "password123456" }
      oauth_apps(:acme_slack).update!(client_secret: "slack-secret")
      oauth_apps(:acme_google).update!(client_secret: "google-secret")
    end

    test "index renders matched principal credentials" do
      principal = principals(:acme_user_alice)
      principal.update!(labels: principal.labels.merge("email" => "alice@example.com"))
      slack = create_credential(oauth_apps(:acme_slack), "slack-sub-alice", "alice@example.com")
      google = create_credential(oauth_apps(:acme_google), "google-sub-alice", "alice@example.com")
      wrap(slack)
      wrap(google)

      get console_principal_reconciliation_url
      assert_response :ok
      assert_select "h1", text: "Principal Reconciliation"
      assert_select "a[href=?]", console_principal_path(principal.oid)
      assert_select "a[href=?]", console_credential_path(slack.oid)
      assert_select "a[href=?]", console_credential_path(google.oid)
      assert_select "form[action=?]", console_principal_reconciliation_apply_path do
        assert_select "input[name=principal_id][value=?]", principal.oid
        assert_select "input[name='credential_ids[]'][value=?]", slack.oid
        assert_select "input[name='credential_ids[]'][value=?]", google.oid
      end
    end

    test "apply grants matched credentials" do
      principal = principals(:acme_user_alice)
      principal.update!(labels: principal.labels.merge("email" => "alice@example.com"))
      slack = create_credential(oauth_apps(:acme_slack), "slack-sub-alice", "alice@example.com")
      google = create_credential(oauth_apps(:acme_google), "google-sub-alice", "alice@example.com")
      wrap(slack)
      wrap(google)

      assert_difference -> { principal.grants.count }, 2 do
        post console_principal_reconciliation_apply_url,
             params: { principal_id: principal.oid, credential_ids: [ slack.oid, google.oid ] }
      end
      assert_redirected_to console_principal_reconciliation_path
      assert_equal "Granted 2 of 2 requested credential tokens.", flash[:notice]
    end

    test "apply rejects an unmatched credential" do
      principal = principals(:acme_user_alice)
      principal.update!(labels: principal.labels.merge("email" => "alice@example.com"))
      other = create_credential(oauth_apps(:acme_google), "google-sub-other", "other@example.com")
      wrap(other)

      assert_no_difference -> { principal.grants.count } do
        post console_principal_reconciliation_apply_url,
             params: { principal_id: principal.oid, credential_ids: [ other.oid ] }
      end
      assert_redirected_to console_principal_reconciliation_path
      assert_match "not a reconciliation candidate", flash[:alert]
    end

    private

    def create_credential(app, subject, email)
      BrokerCredential.create!(
        namespace: app.credential_namespace,
        oauth_app: app,
        provider_subject: subject,
        provider_email: email,
        token_endpoint: app.provider_strategy.token_endpoint,
        refresh_token: "refresh-#{subject}",
        access_token: "access-#{subject}",
        expires_at: 1.hour.from_now,
        last_refresh: Time.current,
        external_user_key: "user-#{subject}"
      )
    end

    def wrap(credential)
      StaticSecret.create!(
        namespace: credential.namespace,
        name: "#{credential.name || credential.provider_subject} token",
        inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
        broker_credential: credential
      )
    end
  end
end
