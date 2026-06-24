require "test_helper"

class PrincipalCredentialReconciliationTest < ActiveSupport::TestCase
  setup do
    oauth_apps(:acme_slack).update!(client_secret: "slack-secret")
    oauth_apps(:acme_google).update!(client_secret: "google-secret")
  end

  test "automatically grants matched Slack and Google wrapper secrets when wrappers appear" do
    principal = principals(:acme_user_alice)
    principal.update!(labels: principal.labels.merge("email" => "alice@example.com"))
    slack = create_credential(oauth_apps(:acme_slack), "slack-sub-alice", "Alice@Example.com")
    google = create_credential(oauth_apps(:acme_google), "google-sub-alice", "alice@example.com")

    assert_difference -> { principal.grants.count }, 2 do
      @slack_secret = wrap(slack)
      @google_secret = wrap(google)
    end

    assert principal.grants.exists?(static_secret: @slack_secret)
    assert principal.grants.exists?(static_secret: @google_secret)

    entry = PrincipalCredentialReconciliation.new.entries.find do |candidate|
      candidate.principal == principal
    end
    assert_not_nil entry
    assert_equal [ slack ], entry.slack_credentials
    assert_equal [ google ], entry.google_credentials
    assert_empty entry.actionable_credentials
  end

  test "automatically grants existing matched wrapper secrets when principal labels change" do
    principal = principals(:acme_user_alice)
    slack = create_credential(oauth_apps(:acme_slack), "slack-sub-alice", "alice@example.com")
    google = create_credential(oauth_apps(:acme_google), "google-sub-alice", "alice@example.com")
    slack_secret = wrap(slack)
    google_secret = wrap(google)

    assert_difference -> { principal.grants.count }, 2 do
      principal.update!(labels: principal.labels.merge("email" => "alice@example.com"))
    end

    assert principal.grants.exists?(static_secret: slack_secret)
    assert principal.grants.exists?(static_secret: google_secret)
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

    entry = PrincipalCredentialReconciliation.new.entries.find do |candidate|
      candidate.principal == principal
    end

    assert_not_nil entry
    assert_equal [ slack ], entry.slack_credentials
    assert_equal [ google ], entry.google_credentials
    assert principal.grants.exists?(static_secret: slack.static_secret)
    assert principal.grants.exists?(static_secret: google.static_secret)
    refute principal.grants.exists?(static_secret: email_only_slack.static_secret)
    refute principal.grants.exists?(static_secret: email_only_google.static_secret)
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
    secret = wrap(mismatched)

    entry = PrincipalCredentialReconciliation.new.entries.find do |candidate|
      candidate.principal == principal
    end

    assert_nil entry
    refute principal.grants.exists?(static_secret: secret)
  end

  test "credential identity update grants an existing wrapper when it becomes a match" do
    principal = principals(:acme_user_alice)
    principal.update!(labels: principal.labels.merge("email" => "alice@example.com"))
    credential = create_credential(oauth_apps(:acme_google), "google-sub-alice", nil)
    secret = wrap(credential)

    assert_no_difference -> { principal.grants.count } do
      PrincipalCredentialReconciliation.new.apply_for_credential(credential)
    end

    assert_difference -> { principal.grants.count }, 1 do
      credential.update!(provider_email: "alice@example.com")
    end
    assert principal.grants.exists?(static_secret: secret)
  end

  test "automatic grant is idempotent" do
    principal = principals(:acme_user_alice)
    principal.update!(labels: principal.labels.merge("email" => "alice@example.com"))
    credential = create_credential(
      oauth_apps(:acme_google),
      "google-sub-alice",
      "alice@example.com"
    )
    secret = wrap(credential)

    assert principal.grants.exists?(static_secret: secret)
    assert_no_difference -> { principal.grants.count } do
      result = PrincipalCredentialReconciliation.new.apply_for_credential(credential)
      assert_equal({ requested: 1, created: 0 }, result)
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
