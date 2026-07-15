module Oauth
  # Level-triggered backfill for GitHub credentials created before enrichment
  # persisted the account login. Work happens in the background, never in a
  # Console chat request. The attempt marker prevents invalid legacy tokens
  # from being retried on every recurring tick; reconnecting still directly
  # enqueues EnrichGithubCredentialIdentityJob through the OAuth flow.
  class PollGithubIdentityEnrichmentJob < ApplicationJob
    queue_as :default

    BATCH_SIZE = 100
    ATTEMPTED_LABEL = "github_login_backfill_attempted_at".freeze

    def perform
      credentials.each do |credential|
        credential.with_lock do
          next if credential.labels&.[](GithubRequesterIdentity::LOGIN_LABEL).present?
          next if credential.labels&.[](ATTEMPTED_LABEL).present?

          EnrichGithubCredentialIdentityJob.perform_later(credential.id)
          credential.update!(
            labels: (credential.labels || {}).merge(ATTEMPTED_LABEL => Time.current.iso8601)
          )
        end
      end
    end

    private

    def credentials
      BrokerCredential
        .joins(:oauth_app)
        .where(oauth_apps: { provider: Providers::Github::KEY })
        .where(dead: false)
        .where.not(access_token: [ nil, "" ])
        .where("coalesce(broker_credentials.labels ->> ?, '') = ''",
               GithubRequesterIdentity::LOGIN_LABEL)
        .where("coalesce(broker_credentials.labels ->> ?, '') = ''", ATTEMPTED_LABEL)
        .order(:id)
        .limit(BATCH_SIZE)
    end
  end
end
