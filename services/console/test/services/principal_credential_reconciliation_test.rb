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
    assert_equal "google-sub-alice", principal.reload.labels["google_subject"]
    assert_equal "alice@example.com", principal.labels["google_email"]

    entry = PrincipalCredentialReconciliation.new.entries.find do |candidate|
      candidate.principal == principal
    end
    assert_not_nil entry
    assert_equal [ slack ], entry.credentials_for("slack")
    assert_equal [ google ], entry.credentials_for("google")
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
    assert_equal [ slack ], entry.credentials_for("slack")
    assert_equal [ google ], entry.credentials_for("google")
    assert principal.grants.exists?(static_secret: slack.static_secret)
    assert principal.grants.exists?(static_secret: google.static_secret)
    assert_equal "google-sub-alice", principal.reload.labels["google_subject"]
    assert_equal "wrong-google@example.com", principal.labels["google_email"]
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
    assert_equal "google-sub-alice", principal.reload.labels["google_subject"]
    assert_equal "alice@example.com", principal.labels["google_email"]
  end

  test "does not overwrite an existing Google subject label" do
    principal = principals(:acme_user_alice)
    principal.update!(
      labels: principal.labels.merge(
        "google_subject" => "google-sub-existing",
        "email" => "alice@example.com"
      )
    )
    credential = create_credential(
      oauth_apps(:acme_google),
      "google-sub-other",
      "alice@example.com"
    )
    secret = wrap(credential)

    entry = PrincipalCredentialReconciliation.new.entries.find do |candidate|
      candidate.principal == principal
    end
    assert_nil entry
    refute principal.grants.exists?(static_secret: secret)
    assert_equal "google-sub-existing", principal.reload.labels["google_subject"]
    assert_nil principal.labels["google_email"]
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

  test "console user principal is granted matching credentials across providers on create" do
    slack = create_credential(oauth_apps(:acme_slack), "slack-sub-member", "member@acme.example")
    google = create_credential(oauth_apps(:acme_google), "google-sub-member", "member@acme.example")
    github = create_credential(oauth_apps(:acme_github), "12345", "member@acme.example")
    secrets = [ slack, google, github ].map { |credential| wrap(credential) }

    principal = create_console_user_principal(users(:member_user), foreign_id: "console-user-member-0")

    secrets.each do |secret|
      assert principal.grants.exists?(static_secret: secret),
             "expected grant for #{secret.name}"
    end
  end

  test "console user principal ignores a spoofed email label" do
    credential = create_credential(oauth_apps(:acme_slack), "slack-sub-carol", "carol@acme.example")
    secret = wrap(credential)

    principal = create_console_user_principal(
      users(:member_user),
      email: "carol@acme.example",
      foreign_id: "console-user-spoofed-email"
    )

    refute principal.grants.exists?(static_secret: secret)
  end

  test "console user principal ignores provider subject labels" do
    credential = create_credential(oauth_apps(:acme_google), "google-sub-carol", "carol@acme.example")
    secret = wrap(credential)

    principal = create_console_user_principal(
      users(:member_user),
      extra_labels: { "google_subject" => "google-sub-carol" },
      foreign_id: "console-user-spoofed-subject"
    )

    refute principal.grants.exists?(static_secret: secret)
  end

  test "console user principal does not accumulate provider labels from matched credentials" do
    create_credential(oauth_apps(:acme_google), "google-sub-member", "member@acme.example")

    principal = create_console_user_principal(users(:member_user), foreign_id: "console-user-labels")

    labels = principal.reload.labels
    assert_nil labels["google_subject"]
    assert_nil labels["google_email"]
  end

  test "console user principal matches credentials via verified identity emails" do
    user = users(:member_user)
    user.user_identities.create!(
      provider: "google", subject: "google-sub-member",
      email: "member.alt@acme.example", email_verified: true
    )
    credential = create_credential(oauth_apps(:acme_slack), "slack-sub-alt", "member.alt@acme.example")
    secret = wrap(credential)

    principal = create_console_user_principal(user, foreign_id: "console-user-member")

    assert principal.grants.exists?(static_secret: secret)
  end

  test "console user principal ignores unverified identity emails" do
    user = users(:member_user)
    user.user_identities.create!(
      provider: "google", subject: "google-sub-unverified",
      email: "victim@acme.example", email_verified: false
    )
    credential = create_credential(oauth_apps(:acme_slack), "slack-sub-victim", "victim@acme.example")
    secret = wrap(credential)

    principal = create_console_user_principal(user, foreign_id: "console-user-member-2")

    refute principal.grants.exists?(static_secret: secret)
  end

  test "github credential identity enrichment grants an existing wrapper when it becomes a match" do
    principal = principals(:acme_user_alice)
    principal.update!(labels: principal.labels.merge("email" => "alice@example.com"))
    credential = create_credential(oauth_apps(:acme_github), "gh-pending", nil)
    secret = wrap(credential)
    refute principal.grants.exists?(static_secret: secret)

    assert_difference -> { principal.grants.count }, 1 do
      credential.update!(provider_email: "alice@example.com")
    end
    assert principal.grants.exists?(static_secret: secret)
  end

  private

  # Mirrors the principal shape minted by Mcp::OauthController#principal_for_current_user.
  # The email/extra_labels overrides simulate tampered or stale labels, which
  # matching must ignore for console-user principals.
  def create_console_user_principal(user, foreign_id:, email: nil, extra_labels: {})
    Principal.create!(
      namespace: "acme",
      foreign_id: foreign_id,
      name: user.name.presence || user.email,
      labels: {
        "managed-by" => "centaur",
        "kind" => "console_user",
        "console-user-id" => user.oid,
        "email" => email || user.email
      }.merge(extra_labels),
      created_by: user
    )
  end

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
