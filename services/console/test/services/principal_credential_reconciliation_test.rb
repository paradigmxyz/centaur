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
        "slack_user_id" => "U0123456789",
        "google_subject" => "google-sub-alice",
        "email" => "alice@example.com"
      )
    )
    slack = create_credential(oauth_apps(:acme_slack), "U0123456789", "wrong-slack@example.com")
    google = create_credential(
      oauth_apps(:acme_google),
      "google-sub-alice",
      "wrong-google@example.com"
    )
    email_only_slack = create_credential(oauth_apps(:acme_slack), "U9999999999", "alice@example.com")
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

  test "changing first-class Slack identity fields grants an existing matching wrapper" do
    principal = principals(:acme_user_alice)
    credential = create_credential(oauth_apps(:acme_slack), "U0123456789", "wrong@example.com")
    credential.update!(labels: { "slack_team_id" => "T0123456789" })
    secret = wrap(credential)
    refute principal.grants.exists?(static_secret: secret)

    assert_difference -> { principal.grants.count }, 1 do
      principal.update!(
        slack_user_id: "U0123456789",
        slack_team_id: "T0123456789"
      )
    end

    assert principal.grants.exists?(static_secret: secret)
    assert_empty principal.reload.labels.slice("slack_user_id", "slack_team_id")
  end

  test "matches Slack credentials with the same team or enterprise scope" do
    [
      [ "team", "U1123456789", "T1123456789" ],
      [ "enterprise", "U2123456789", "E2123456789" ]
    ].each do |name, user_id, scope_id|
      principal = create_slack_user_principal(
        foreign_id: "slack-scope-#{name}",
        slack_user_id: user_id,
        slack_team_id: scope_id
      )
      credential = create_credential(oauth_apps(:acme_slack), user_id, "wrong-#{name}@example.com")
      credential.update!(labels: { "slack_team_id" => scope_id })
      secret = wrap(credential)

      assert principal.grants.exists?(static_secret: secret),
             "expected matching #{name} scope to grant the credential"
    end
  end

  test "requires both ordinary principal and Slack credential to carry the same scope" do
    cases = [
      {
        name: "neither side scoped",
        user_id: "U3123456789",
        principal_scope: nil,
        credential_scope: nil,
        granted: true
      },
      {
        name: "principal only scoped",
        user_id: "U4123456789",
        principal_scope: "T4123456789",
        credential_scope: nil,
        granted: false
      },
      {
        name: "credential only scoped",
        user_id: "U5123456789",
        principal_scope: nil,
        credential_scope: "T5123456789",
        granted: false
      }
    ]

    cases.each do |test_case|
      principal = create_slack_user_principal(
        foreign_id: "slack-scope-#{test_case[:user_id].downcase}",
        slack_user_id: test_case[:user_id],
        slack_team_id: test_case[:principal_scope]
      )
      credential = create_credential(
        oauth_apps(:acme_slack),
        test_case[:user_id],
        "wrong-#{test_case[:user_id].downcase}@example.com"
      )
      if test_case[:credential_scope]
        credential.update!(labels: { "slack_team_id" => test_case[:credential_scope] })
      end
      secret = wrap(credential)

      assert_equal test_case[:granted], principal.grants.exists?(static_secret: secret),
                   "unexpected result when #{test_case[:name]}"
    end
  end

  test "requires matching Slack team labels when either side carries one" do
    principal = principals(:acme_user_alice)
    principal.update!(
      labels: principal.labels.merge(
        "slack_team_id" => "T0123456789",
        "slack_user_id" => "U0123456789"
      )
    )
    mismatched = create_credential(oauth_apps(:acme_slack), "U0123456789", "alice-alt@example.com")
    mismatched.update!(labels: { "slack_team_id" => "T9999999999" })
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

  test "console user principal syncs Slack fields from a matched admin credential" do
    app = oauth_apps(:acme_slack)
    app.update!(labels: app.labels.merge("slack_team_id" => "T0123456789"))
    credential = create_credential(app, "U0123456789", "member@acme.example")
    secret = wrap(credential)

    principal = create_console_user_principal(users(:member_user), foreign_id: "console-user-slack")

    assert principal.grants.exists?(static_secret: secret)
    assert_equal "U0123456789", principal.reload.slack_user_id
    assert_equal "T0123456789", principal.slack_team_id
    assert_empty principal.labels.slice("slack_user_id", "slack_team_id")
  end

  test "console user creation grants but does not sync malformed Slack credential identity" do
    credential = create_credential(oauth_apps(:acme_slack), "U12345", "member@acme.example")
    credential.update!(labels: { "slack_team_id" => "T0123456789" })
    secret = wrap(credential)

    principal = create_console_user_principal(
      users(:member_user),
      foreign_id: "console-user-malformed-slack-on-create"
    )

    assert principal.grants.exists?(static_secret: secret)
    assert_nil principal.reload.slack_user_id
    assert_nil principal.slack_team_id
  end

  test "wrapper creation grants but does not sync malformed Slack credential identity" do
    principal = create_console_user_principal(
      users(:member_user),
      foreign_id: "console-user-malformed-slack-on-wrap"
    )
    credential = create_credential(oauth_apps(:acme_slack), "U0123456789", "member@acme.example")
    credential.update!(labels: { "slack_team_id" => "TACME" })

    secret = nil
    assert_nothing_raised { secret = wrap(credential) }

    assert principal.grants.exists?(static_secret: secret)
    assert_nil principal.reload.slack_user_id
    assert_nil principal.slack_team_id
  end

  test "credential update grants but does not sync malformed Slack credential identity" do
    principal = create_console_user_principal(
      users(:member_user),
      foreign_id: "console-user-malformed-slack-on-credential-update"
    )
    credential = create_credential(oauth_apps(:acme_slack), "U12345", nil)
    credential.update!(labels: { "slack_team_id" => "TACME" })
    secret = wrap(credential)

    assert_nothing_raised { credential.update!(provider_email: "member@acme.example") }

    assert principal.grants.exists?(static_secret: secret)
    assert_nil principal.reload.slack_user_id
    assert_nil principal.slack_team_id
  end

  test "console user Slack scope bootstraps once and then rejects another scope" do
    first = create_credential(oauth_apps(:acme_slack), "U6123456789", "member@acme.example")
    first.update!(labels: { "slack_team_id" => "T6123456789" })
    first_secret = wrap(first)

    principal = create_console_user_principal(users(:member_user), foreign_id: "console-user-pinned-slack")
    assert principal.grants.exists?(static_secret: first_secret)
    assert_equal "U6123456789", principal.reload.slack_user_id
    assert_equal "T6123456789", principal.slack_team_id

    second = create_credential(oauth_apps(:acme_slack), "U7123456789", "member@acme.example")
    second.update!(labels: { "slack_team_id" => "T7123456789" })
    second_secret = wrap(second)

    refute principal.grants.exists?(static_secret: second_secret)
    assert_equal "U6123456789", principal.reload.slack_user_id
    assert_equal "T6123456789", principal.slack_team_id
  end

  test "console user principal declines Slack identity sync when matches are ambiguous" do
    first = create_credential(oauth_apps(:acme_slack), "U8123456789", "member@acme.example")
    first.update!(labels: { "slack_team_id" => "T8123456789" })
    second = create_credential(oauth_apps(:acme_slack), "U9123456789", "member@acme.example")
    second.update!(labels: { "slack_team_id" => "T9123456789" })
    secrets = [ wrap(first), wrap(second) ]

    principal = create_console_user_principal(users(:member_user), foreign_id: "console-user-ambiguous-slack")

    secrets.each do |secret|
      assert principal.grants.exists?(static_secret: secret)
    end
    assert_nil principal.reload.slack_user_id
    assert_nil principal.slack_team_id
  end

  test "matches credentials through first-class Slack email" do
    principal = Principal.create!(
      namespace: "acme",
      foreign_id: "slack-email-user",
      slack_email: "member.slack@example.com",
      created_by: users(:acme_admin)
    )
    credential = create_credential(
      oauth_apps(:acme_slack),
      "U10123456789",
      "Member.Slack@Example.com"
    )
    secret = wrap(credential)

    assert principal.grants.exists?(static_secret: secret)
    assert_empty principal.reload.labels.slice("slack_email")
  end

  test "does not reconcile matching Slack identities across namespaces" do
    principal = create_slack_user_principal(
      namespace: "globex",
      foreign_id: "globex-slack-user",
      slack_user_id: "U11123456789",
      slack_team_id: "T11123456789",
      created_by: users(:globex_admin)
    )
    credential = create_credential(oauth_apps(:acme_slack), "U11123456789", "same@example.com")
    credential.update!(labels: { "slack_team_id" => "T11123456789" })
    secret = wrap(credential)

    assert_equal(
      { requested: 0, created: 0 },
      PrincipalCredentialReconciliation.new.apply_for_principal(principal)
    )
    refute principal.grants.exists?(static_secret: secret)
  end

  test "console user principal ignores a spoofed cached email" do
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
  # The email/extra_labels overrides simulate tampered or stale cached identity,
  # which matching must ignore for console-user principals.
  def create_console_user_principal(user, foreign_id:, email: nil, extra_labels: {})
    Principal.create!(
      namespace: "acme",
      foreign_id: foreign_id,
      name: user.name.presence || user.email,
      kind: "console_user",
      console_user_id: user.id,
      console_user_email: email || user.email,
      labels: {
        "managed-by" => "centaur"
      }.merge(extra_labels),
      created_by: user
    )
  end

  def create_slack_user_principal(
    foreign_id:,
    slack_user_id:,
    slack_team_id:,
    namespace: "acme",
    created_by: users(:acme_admin)
  )
    Principal.create!(
      namespace: namespace,
      foreign_id: foreign_id,
      kind: PrincipalCredentialReconciliation::USER_KIND,
      slack_user_id: slack_user_id,
      slack_team_id: slack_team_id,
      created_by: created_by
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
