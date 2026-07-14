require "test_helper"

module Granola
  class JobsTest < ActiveJob::TestCase
    def create_credential(app:, access_token: "token", dead: false)
      BrokerCredential.create!(
        oauth_app: app,
        namespace: "acme",
        foreign_id: "granola-job-#{SecureRandom.hex(6)}",
        token_endpoint: app.provider_strategy.token_endpoint,
        access_token: access_token,
        refresh_token: "refresh",
        last_refresh: Time.current,
        expires_at: 1.hour.from_now,
        scopes: %w[meetings:read],
        provider_subject: "granola-subject-#{SecureRandom.hex(4)}",
        provider_email: "person@example.com",
        dead: dead
      )
    end

    def create_granola_app(enabled: true, slug: "granola")
      OauthApp.create!(
        provider: "granola",
        slug: "#{slug}-#{SecureRandom.hex(6)}",
        client_id: "granola-client",
        client_secret: "granola-secret",
        allowed_scopes: %w[meetings:read],
        credential_namespace: "acme",
        enabled: enabled,
        created_by: users(:acme_admin)
      )
    end

    test "poll job only enqueues live credentials for the configured Granola app" do
      expected_app = create_granola_app(slug: "granola-sync")
      expected = create_credential(app: expected_app)
      create_credential(app: expected_app, dead: true)
      create_credential(app: create_granola_app(slug: "granola-disabled", enabled: false))
      create_credential(app: create_granola_app(slug: "another-granola-app"))

      PollSyncJob.perform_now(expected_app.slug)

      enqueued_ids = enqueued_jobs
        .select { |job| job[:job] == SyncCredentialJob }
        .map { |job| job[:args].first }
      assert_equal [ expected.id ], enqueued_ids
    end
  end
end
