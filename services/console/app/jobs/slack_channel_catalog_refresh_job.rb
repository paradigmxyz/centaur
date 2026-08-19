class SlackChannelCatalogRefreshJob < ApplicationJob
  def perform(cache_key)
    SlackChannelCatalogProvider.refresh(cache_key: cache_key)
  end
end
