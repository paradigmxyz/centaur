require "test_helper"

class SlackDeliveryPolicyTest < ActiveSupport::TestCase
  test "allows the user's Slack DM and channels with effective upload permission" do
    user = users(:acme_admin)
    user.user_identities.create!(
      provider: "slack",
      subject: "U0123456789",
      team_id: "T0123456789"
    )
    principal = ConsoleUserPrincipalProvisioner.call(user)
    role = Role.create!(name: "Slack delivery", foreign_id: "slack-delivery", created_by: user)
    principal.roles << role
    role.slack_channel_permissions.create!(
      channel_id: "C0123456789",
      upload_enabled: true
    )

    policy = SlackDeliveryPolicy.new(user)

    assert policy.allowed?("U0123456789")
    assert policy.allowed?("C0123456789")
    assert_not policy.allowed?("C9999999999")
  end
end
