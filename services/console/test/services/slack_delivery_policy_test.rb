require "test_helper"

class SlackDeliveryPolicyTest < ActiveSupport::TestCase
  test "allows the user's Slack DM and channels shared with the bot" do
    user = users(:acme_admin)
    user.user_identities.create!(
      provider: "slack",
      subject: "U0123456789",
      team_id: "T0123456789"
    )
    policy = SlackDeliveryPolicy.new(user)

    assert policy.allowed?("U0123456789")
    assert policy.allowed?("C0123456789")
    assert_not policy.allowed?("G9876543210")
    assert_not policy.allowed?("C9999999999")
  end
end
