class SlackChannelCatalogMembershipRefreshJob < ApplicationJob
  MAX_RETRYABLE_EXECUTIONS = 3

  queue_as :default

  limits_concurrency to: 1, key: ->(*) { "slack_channel_catalog_membership" }

  def perform(channel_id = nil)
    SlackChannelCatalogProvider.refresh_memberships(channel_id: channel_id)
  rescue SlackChannelCatalog::RetryableApiError => e
    if executions >= MAX_RETRYABLE_EXECUTIONS
      Rails.logger.warn("Slack channel membership refresh dropped after repeated retryable API failures")
      return
    end

    retry_job wait: e.retry_after.seconds, error: e
  end
end
