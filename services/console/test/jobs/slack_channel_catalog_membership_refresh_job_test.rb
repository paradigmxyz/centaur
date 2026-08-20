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

    SlackChannelCatalogSync.stub(:new, sync) do
      SlackChannelCatalogMembershipRefreshJob.perform_now("C0123456789")
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

    SlackChannelCatalogSync.stub(:new, sync) do
      SlackChannelCatalogMembershipRefreshJob.perform_now
    end

    assert called
  end
end
