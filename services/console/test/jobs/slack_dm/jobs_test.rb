require "test_helper"

module SlackDm
  class JobsTest < ActiveJob::TestCase
    def slack_app(slug: "slack-dms")
      OauthApp.create!(
        provider: "slack",
        slug: slug,
        client_id: "slack-client-#{SecureRandom.hex(4)}",
        client_secret: "secret",
        allowed_scopes: SlackDm::SyncCredential::REQUIRED_SCOPES,
        credential_namespace: "acme",
        created_by: users(:acme_admin)
      )
    end

    def slack_credential(
      app:,
      scopes: SlackDm::SyncCredential::REQUIRED_SCOPES,
      access_token: "xoxp-live",
      provider_subject: "U#{SecureRandom.hex(4).upcase}"
    )
      BrokerCredential.create!(
        oauth_app: app,
        namespace: "acme",
        foreign_id: "slack-dms-#{SecureRandom.hex(6)}",
        token_endpoint: "https://slack.com/api/oauth.v2.access",
        access_token: access_token,
        refresh_token: "refresh",
        last_refresh: Time.current,
        expires_at: 1.hour.from_now,
        scopes: scopes,
        provider_subject: provider_subject
      )
    end

    test "PollSyncJob enqueues credentials with any supported private conversation scopes" do
      app = slack_app
      good = slack_credential(app: app)
      dm_only = slack_credential(app: app, scopes: SlackDm::SyncCredential::DM_REQUIRED_SCOPES)
      missing_scope = slack_credential(app: app, scopes: %w[chat:write])
      no_token = slack_credential(app: app, access_token: nil)
      other_app = slack_app(slug: "other-slack")
      other = slack_credential(app: other_app)

      SlackDm::PollSyncJob.perform_now("slack-dms")

      enqueued_ids = enqueued_jobs
        .select { |job| job[:job] == SlackDm::SyncCredentialJob }
        .map { |job| job[:args].first }
      assert_includes enqueued_ids, good.id
      assert_includes enqueued_ids, dm_only.id
      refute_includes enqueued_ids, missing_scope.id
      refute_includes enqueued_ids, no_token.id
      refute_includes enqueued_ids, other.id
    end

    test "SyncCredentialJob is a no-op for missing credentials" do
      assert_nothing_raised { SlackDm::SyncCredentialJob.perform_now(-1) }
    end
  end
end
