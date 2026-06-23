require "test_helper"

module Console
  class PrincipalCredentialReconciliationTest < ActiveSupport::TestCase
    setup do
      @operator = users(:acme_admin)
      oauth_apps(:acme_slack).update!(client_secret: "slack-secret")
      oauth_apps(:acme_google).update!(client_secret: "google-secret")
    end

    test "matches a user principal to Slack and Google credentials for the same email" do
      principal = principals(:acme_user_alice)
      principal.update!(labels: principal.labels.merge("email" => "alice@example.com"))
      slack = create_credential(oauth_apps(:acme_slack), "slack-sub-alice", "Alice@Example.com")
      google = create_credential(oauth_apps(:acme_google), "google-sub-alice", "alice@example.com")
      wrap(slack)
      wrap(google)

      entry = Console::PrincipalCredentialReconciliation.new.entries.find do |candidate|
        candidate.principal == principal
      end

      assert_not_nil entry
      assert_equal [ slack ], entry.slack_credentials
      assert_equal [ google ], entry.google_credentials
      assert_equal [ slack, google ], entry.actionable_credentials
    end

    test "matches provider subjects before falling back to email labels" do
      principal = principals(:acme_user_alice)
      principal.update!(
        labels: principal.labels.merge(
          "slack_user_id" => "U12345",
          "google_subject" => "google-sub-alice",
          "email" => "alice@example.com"
        )
      )
      slack = create_credential(oauth_apps(:acme_slack), "U12345", "wrong-slack@example.com")
      google = create_credential(
        oauth_apps(:acme_google),
        "google-sub-alice",
        "wrong-google@example.com"
      )
      email_only_slack = create_credential(oauth_apps(:acme_slack), "U99999", "alice@example.com")
      email_only_google = create_credential(
        oauth_apps(:acme_google),
        "google-sub-other",
        "alice@example.com"
      )
      [ slack, google, email_only_slack, email_only_google ].each { |credential| wrap(credential) }

      entry = Console::PrincipalCredentialReconciliation.new.entries.find do |candidate|
        candidate.principal == principal
      end

      assert_not_nil entry
      assert_equal [ slack ], entry.slack_credentials
      assert_equal [ google ], entry.google_credentials
    end

    test "requires matching Slack team labels when either side carries one" do
      principal = principals(:acme_user_alice)
      principal.update!(
        labels: principal.labels.merge(
          "slack_team_id" => "T123",
          "slack_user_id" => "U12345"
        )
      )
      mismatched = create_credential(oauth_apps(:acme_slack), "U12345", "alice-alt@example.com")
      mismatched.update!(labels: { "slack_team_id" => "T999" })
      wrap(mismatched)

      entry = Console::PrincipalCredentialReconciliation.new.entries.find do |candidate|
        candidate.principal == principal
      end

      assert_nil entry
    end

    test "apply grants each matched wrapper secret idempotently" do
      principal = principals(:acme_user_alice)
      principal.update!(labels: principal.labels.merge("email" => "alice@example.com"))
      slack = create_credential(oauth_apps(:acme_slack), "slack-sub-alice", "alice@example.com")
      google = create_credential(oauth_apps(:acme_google), "google-sub-alice", "alice@example.com")
      slack_secret = wrap(slack)
      google_secret = wrap(google)

      reconciliation = Console::PrincipalCredentialReconciliation.new
      assert_difference -> { principal.grants.count }, 2 do
        result = reconciliation.apply(
          principal_oid: principal.oid,
          credential_oids: [ slack.oid, google.oid ],
          current_user: @operator
        )
        assert_equal({ requested: 2, created: 2 }, result)
      end

      assert principal.grants.exists?(static_secret: slack_secret)
      assert principal.grants.exists?(static_secret: google_secret)

      assert_no_difference -> { principal.grants.count } do
        result = Console::PrincipalCredentialReconciliation.new.apply(
          principal_oid: principal.oid,
          credential_oids: [ slack.oid, google.oid ],
          current_user: @operator
        )
        assert_equal({ requested: 2, created: 0 }, result)
      end
    end

    test "apply rejects credentials that are not proposed for the principal" do
      principal = principals(:acme_user_alice)
      principal.update!(labels: principal.labels.merge("email" => "alice@example.com"))
      other = create_credential(oauth_apps(:acme_google), "google-sub-other", "other@example.com")
      wrap(other)

      assert_raises(ActiveRecord::RecordNotFound) do
        Console::PrincipalCredentialReconciliation.new.apply(
          principal_oid: principal.oid,
          credential_oids: [ other.oid ],
          current_user: @operator
        )
      end
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
