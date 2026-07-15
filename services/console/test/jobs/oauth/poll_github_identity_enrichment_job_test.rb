require "test_helper"

module Oauth
  class PollGithubIdentityEnrichmentJobTest < ActiveJob::TestCase
    test "enqueues legacy GitHub credentials once and marks the attempt" do
      credential = github_credential(labels: {})

      assert_enqueued_with(job: EnrichGithubCredentialIdentityJob, args: [ credential.id ]) do
        PollGithubIdentityEnrichmentJob.perform_now
      end

      assert credential.reload.labels[PollGithubIdentityEnrichmentJob::ATTEMPTED_LABEL].present?

      assert_no_enqueued_jobs only: EnrichGithubCredentialIdentityJob do
        PollGithubIdentityEnrichmentJob.perform_now
      end
    end

    test "skips credentials whose login is already enriched" do
      github_credential(labels: { "github_login" => "goksu" })

      assert_no_enqueued_jobs only: EnrichGithubCredentialIdentityJob do
        PollGithubIdentityEnrichmentJob.perform_now
      end
    end

    private

    def github_credential(labels:)
      app = oauth_apps(:acme_github)
      app.update!(client_secret: "github-secret")
      BrokerCredential.create!(
        namespace: app.credential_namespace,
        oauth_app: app,
        created_by: users(:member_user),
        provider_subject: SecureRandom.random_number(1_000_000).to_s,
        provider_email: users(:member_user).email,
        labels: labels,
        token_endpoint: app.provider_strategy.token_endpoint,
        access_token: "gho-legacy",
        scopes: %w[repo]
      )
    end
  end
end
