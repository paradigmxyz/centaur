class SlackChannelCatalogMembershipRefreshJob < ApplicationJob
  MAX_RETRYABLE_EXECUTIONS = 3

  queue_as :default

  limits_concurrency to: 1, key: ->(*) { "slack_channel_catalog_membership" }

  def perform(channel_id = nil)
    return unless SlackChannelCatalogSync.configured?

    sync = SlackChannelCatalogSync.new
    if channel_id
      channel = sync.import_channel(channel_id)
      sync.channel_members(channel.channel_id) if channel
    else
      sync.sync_memberships
    end
  rescue SlackApi::RetryableError => e
    if executions >= MAX_RETRYABLE_EXECUTIONS
      Rails.logger.warn("Slack channel membership refresh dropped after repeated retryable API failures")
      return
    end

    retry_job wait: e.retry_after.seconds, error: e
  end
end
