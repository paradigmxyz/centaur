require "test_helper"

class SlackChannelCatalogRefreshJobTest < ActiveJob::TestCase
  test "refreshes the requested cache entry through the provider" do
    cache_key = "slack_channel_catalog/v2/api-digest/token-digest"
    refreshed_key = nil
    refresh = ->(cache_key:) { refreshed_key = cache_key }

    SlackChannelCatalogProvider.stub(:refresh, refresh) do
      SlackChannelCatalogRefreshJob.perform_now(cache_key)
    end

    assert_equal cache_key, refreshed_key
  end
end
