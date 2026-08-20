require "test_helper"

class SlackChannelCatalogMembershipRefreshJobTest < ActiveJob::TestCase
  test "passes an optional channel to the provider" do
    refreshed = nil
    refresh = ->(channel_id:) { refreshed = channel_id }

    SlackChannelCatalogProvider.stub(:refresh_memberships, refresh) do
      SlackChannelCatalogMembershipRefreshJob.perform_now("C0123456789")
    end

    assert_equal "C0123456789", refreshed
  end
end
