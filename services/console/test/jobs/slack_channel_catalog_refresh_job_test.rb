require "test_helper"

class SlackChannelCatalogRefreshJobTest < ActiveJob::TestCase
  test "syncs the channel list" do
    called = false
    sync = Object.new
    sync.define_singleton_method(:sync_channels) { called = true }

    SlackChannelCatalogSync.stub(:new, sync) do
      Rails.cache.stub(:delete, true) { SlackChannelCatalogRefreshJob.perform_now }
    end

    assert called
  end
end
