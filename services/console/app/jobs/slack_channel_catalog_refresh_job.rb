class SlackChannelCatalogRefreshJob < ApplicationJob
  MAX_RETRYABLE_EXECUTIONS = 3

  queue_as :default

  limits_concurrency to: 1, key: -> { "slack_channel_catalog_discovery" }

  def perform
    return unless SlackChannelCatalogSync.configured?

    SlackChannelCatalogSync.new.sync_channels
  rescue SlackApi::RetryableError => e
    if executions >= MAX_RETRYABLE_EXECUTIONS
      Rails.logger.warn("Slack channel catalog discovery dropped after repeated retryable API failures")
      return
    end

    retry_job wait: e.retry_after.seconds, error: e
  ensure
    Rails.cache.delete(SlackChannelCatalogSync::REFRESH_LOCK_KEY)
  end
end
