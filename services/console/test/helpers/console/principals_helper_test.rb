require "test_helper"

module Console
  class PrincipalsHelperTest < ActionView::TestCase
    test "slack_channel_options_for_permission preserves a missing current channel" do
      permission = SlackChannelPermission.new(channel_id: "C0123456789", channel_name: "general")
      options = [ [ "#random (C9999999999)", "C9999999999" ] ]

      assert_equal(
        [
          [ "#general (C0123456789)", "C0123456789" ],
          [ "#random (C9999999999)", "C9999999999" ]
        ],
        slack_channel_options_for_permission(permission, options)
      )
    end

    test "slack_channel_options_for_permission leaves existing channel options unchanged" do
      permission = SlackChannelPermission.new(channel_id: "C0123456789", channel_name: "general")
      options = [ [ "#general (C0123456789)", "C0123456789" ] ]

      assert_equal options, slack_channel_options_for_permission(permission, options)
    end
  end
end
