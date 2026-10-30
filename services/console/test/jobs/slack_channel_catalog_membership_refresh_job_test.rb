require "test_helper"

class SlackChannelCatalogMembershipRefreshJobTest < ActiveJob::TestCase
  test "imports and refreshes an optional channel" do
    calls = []
    channel = Struct.new(:channel_id).new("C0123456789")
    sync = Object.new
    sync.define_singleton_method(:import_channel) do |channel_id|
      calls << [ :import_channel, channel_id ]
      channel
    end
    sync.define_singleton_method(:channel_members) do |channel_id|
      calls << [ :channel_members, channel_id ]
    end

    SlackChannelCatalogSync.stub(:configured?, true) do
      SlackChannelCatalogSync.stub(:new, sync) do
        SlackChannelCatalogMembershipRefreshJob.perform_now("C0123456789")
      end
    end

    assert_equal(
      [ [ :import_channel, "C0123456789" ], [ :channel_members, "C0123456789" ] ],
      calls
    )
  end

  test "syncs stale memberships without a channel" do
    called = false
    sync = Object.new
    sync.define_singleton_method(:sync_memberships) { called = true }

    SlackChannelCatalogSync.stub(:configured?, true) do
      SlackChannelCatalogSync.stub(:new, sync) do
        SlackChannelCatalogMembershipRefreshJob.perform_now
      end
    end

    assert called
  end

  test "does nothing when the catalog is unconfigured" do
    SlackChannelCatalogSync.stub(:configured?, false) do
      SlackChannelCatalogSync.stub(:new, -> { flunk("must not construct the sync") }) do
        assert_nothing_raised { SlackChannelCatalogMembershipRefreshJob.perform_now }
      end
    end
  end
end
