require "test_helper"

class PrincipalSlackCatalogTest < ActiveJob::TestCase
  test "creating a configured Slack channel principal enqueues a targeted membership refresh" do
    with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-test-token") do
      assert_enqueued_with(job: SlackChannelCatalogMembershipRefreshJob, args: [ "C1111111111" ]) do
        Principal.create!(
          created_by: users(:acme_admin),
          foreign_id: "slack-channel-targeted-refresh",
          kind: "slack_channel",
          slack_channel_id: "C1111111111"
        )
      end
    end
  end

  test "other principals and unconfigured catalogs do not enqueue a refresh" do
    with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => nil, "SLACK_BOT_TOKEN" => nil) do
      assert_no_enqueued_jobs only: SlackChannelCatalogMembershipRefreshJob do
        Principal.create!(
          created_by: users(:acme_admin),
          foreign_id: "slack-channel-unconfigured",
          kind: "slack_channel",
          slack_channel_id: "C2222222222"
        )
        Principal.create!(
          created_by: users(:acme_admin),
          foreign_id: "ordinary-principal",
          kind: "user"
        )
      end
    end
  end
end
