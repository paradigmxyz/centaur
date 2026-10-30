require "test_helper"

class SlackChannelCatalogRefreshJobTest < ActiveJob::TestCase
  test "syncs the channel list" do
    called = false
    sync = Object.new
    sync.define_singleton_method(:sync_channels) { called = true }

    SlackChannelCatalogSync.stub(:configured?, true) do
      SlackChannelCatalogSync.stub(:new, sync) do
        Rails.cache.stub(:delete, true) { SlackChannelCatalogRefreshJob.perform_now }
      end
    end

    assert called
  end

  test "does nothing when the catalog is unconfigured" do
    SlackChannelCatalogSync.stub(:configured?, false) do
      SlackChannelCatalogSync.stub(:new, -> { flunk("must not construct the sync") }) do
        assert_nothing_raised { SlackChannelCatalogRefreshJob.perform_now }
      end
    end
  end
end
