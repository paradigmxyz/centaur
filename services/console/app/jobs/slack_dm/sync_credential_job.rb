module SlackDm
  class SyncCredentialJob < ApplicationJob
    MAX_RETRYABLE_EXECUTIONS = 2

    queue_as :default

    limits_concurrency to: 1, key: ->(credential_id) { "slack_dm_sync_#{credential_id}" }

    def perform(credential_id)
      credential = BrokerCredential.includes(:oauth_app).find_by(id: credential_id)
      return unless credential
      return if credential.dead?
      return if credential.access_token.blank?
      return unless credential.oauth_app&.provider == Oauth::Providers::Slack::KEY
      return unless SlackDm::SyncCredential.required_scopes_granted?(credential.scopes)

      SlackDm::SyncCredential.new(credential).call
    rescue SlackApi::RetryableError => e
      if executions >= MAX_RETRYABLE_EXECUTIONS
        Rails.logger.warn do
          "Slack sync job dropped after repeated retryable API failures: " \
            "credential_id=#{credential_id} executions=#{executions}"
        end
        return
      end

      retry_job wait: e.retry_after.seconds, error: e
    end
  end
end
