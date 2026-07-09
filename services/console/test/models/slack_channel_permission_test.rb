require "test_helper"

class SlackChannelPermissionTest < ActiveSupport::TestCase
  test "normalizes channel id and requires at least one permission" do
    permission = SlackChannelPermission.new(
      principal: principals(:acme_channel),
      channel_id: " c0123456789 ",
      channel_name: " general ",
      upload_enabled: true,
      download_enabled: false,
      history_enabled: false
    )

    assert_predicate permission, :valid?
    permission.save!
    assert_equal "C0123456789", permission.channel_id
    assert_equal "general", permission.channel_name

    empty = SlackChannelPermission.new(
      principal: principals(:acme_channel),
      channel_id: "C9999999999"
    )
    assert_not empty.valid?
    assert_includes empty.errors[:base], "Select at least one Slack permission"
  end

  test "replace_for_principal merges duplicate channel rows" do
    principal = principals(:acme_channel)

    SlackChannelPermission.replace_for_principal!(
      principal,
      {
        "0" => { "channel_id" => "c0123456789", "upload_enabled" => "1", "download_enabled" => "0", "history_enabled" => "0" },
        "1" => { "channel_id" => "C0123456789", "upload_enabled" => "0", "download_enabled" => "1", "history_enabled" => "1" }
      },
      channel_names_by_id: { "C0123456789" => "general" }
    )

    permission = principal.slack_channel_permissions.reload.sole
    assert_equal "C0123456789", permission.channel_id
    assert_equal "general", permission.channel_name
    assert_equal true, permission.upload_enabled
    assert_equal true, permission.download_enabled
    assert_equal true, permission.history_enabled
  end
end
