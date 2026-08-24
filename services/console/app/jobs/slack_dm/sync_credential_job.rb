module SlackDm
  class SyncCredentialJob < ApplicationJob
    CONCURRENCY_DURATION = 6.hours
    RUN_TIME_BUDGET = 30.minutes

    queue_as :default

    limits_concurrency(
      to: 1,
      key: ->(*) { "slack_dm_sync" },
      group: "SlackDmSync",
      duration: CONCURRENCY_DURATION,
      on_conflict: :discard
    )

    def perform(oauth_app_slug = SlackDm::SyncCredential.oauth_app_slug, expected_not_before = nil)
      # Credential IDs were the argument before this became a global cursor job.
      # Ignore any of those jobs that were already queued during deployment.
      return unless oauth_app_slug.is_a?(String)

      cursor = SlackDmSyncCursor.find_or_create_by!(oauth_app_slug: oauth_app_slug)
      return if expected_not_before && cursor.not_before != expected_not_before
      return if cursor.not_before&.future?

      credentials = eligible_credentials(oauth_app_slug)
      if credentials.empty?
        cursor.update!(next_credential_id: nil, next_conversation_id: nil, not_before: nil)
        return
      end

      deadline = RUN_TIME_BUDGET.from_now
      credentials = ordered_credentials(credentials, cursor.next_credential_id)
      credentials.each_with_index do |credential, index|
        break if Time.current >= deadline
        return unless sync_credential(cursor, credential, deadline)

        next_credential = credentials[index + 1] || credentials.first
        cursor.update!(
          next_credential_id: next_credential.id,
          next_conversation_id: nil,
          not_before: nil
        )
      end
    end

    private

    def credentials_for(oauth_app_slug)
      BrokerCredential
        .includes(:oauth_app)
        .joins(:oauth_app)
        .where(dead: false)
        .where(oauth_apps: {
          provider: Oauth::Providers::Slack::KEY,
          slug: oauth_app_slug,
          enabled: true
        })
    end

    def eligible_credentials(oauth_app_slug)
      credentials_for(oauth_app_slug).order(:id).select do |credential|
        credential.access_token.present? &&
          SlackDm::SyncCredential.required_scopes_granted?(credential.scopes)
      end
    end

    def ordered_credentials(credentials, next_credential_id)
      return credentials unless next_credential_id

      start_index = credentials.index { |credential| credential.id >= next_credential_id } || 0
      credentials.rotate(start_index)
    end

    def sync_credential(cursor, credential, deadline)
      SlackDm::SyncCredential.new(credential).call(
        starting_conversation_id: cursor.next_conversation_id,
        deadline: deadline
      ) do |conversation_id|
        cursor.update!(
          next_credential_id: credential.id,
          next_conversation_id: conversation_id,
          not_before: nil
        )
      end
    rescue SlackApi::RateLimitedError => e
      retry_at = e.retry_after.seconds.from_now
      cursor.update!(next_credential_id: credential.id, not_before: retry_at)
      persisted_retry_at = cursor.reload.not_before
      self.class.set(wait_until: persisted_retry_at).perform_later(
        cursor.oauth_app_slug,
        persisted_retry_at
      )
      Rails.logger.info do
        "Slack DM sync paused after rate limit: credential_id=#{credential.id} " \
          "retry_at=#{persisted_retry_at.iso8601}"
      end
      false
    rescue SlackApi::Error => e
      Rails.logger.warn do
        "Slack DM sync failed for credential #{credential.id}: #{e.class}: #{e.message}"
      end
      true
    end
  end
end
