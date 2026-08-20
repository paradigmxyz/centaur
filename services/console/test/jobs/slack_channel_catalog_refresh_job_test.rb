require "test_helper"

class SlackChannelCatalogRefreshJobTest < ActiveJob::TestCase
  test "refreshes channel discovery through the provider" do
    called = false
    SlackChannelCatalogProvider.stub(:refresh, -> { called = true }) do
      SlackChannelCatalogRefreshJob.perform_now
    end

    assert called
  end
end
