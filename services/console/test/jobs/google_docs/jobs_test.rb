require "test_helper"

module GoogleDocs
  class JobsTest < ActiveJob::TestCase
    test "poll job enqueues a credential with drive readonly scope alone" do
      app = OauthApp.create!(
        provider: "google",
        slug: "google-docs-job-#{SecureRandom.hex(6)}",
        client_id: "google-client",
        client_secret: "secret",
        allowed_scopes: [ SyncCredential::DRIVE_READONLY_SCOPE ],
        enabled: true,
        created_by: users(:acme_admin)
      )
      credential = BrokerCredential.create!(
        oauth_app: app,
        foreign_id: "google-docs-job-#{SecureRandom.hex(6)}",
        token_endpoint: "https://oauth2.googleapis.com/token",
        access_token: "token",
        refresh_token: "refresh",
        last_refresh: Time.current,
        expires_at: 1.hour.from_now,
        scopes: [ SyncCredential::DRIVE_READONLY_SCOPE ]
      )

      PollSyncJob.perform_now(app.slug)

      assert_enqueued_with(job: SyncCredentialJob, args: [ credential.id ])
    end
  end
end
