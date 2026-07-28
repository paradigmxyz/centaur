require "test_helper"

module Console
  class SlackChannelPermissionsControllerTest < ActionDispatch::IntegrationTest
    setup do
      @operator = users(:acme_admin)
      post login_url, params: { email: @operator.email, password: "password123456" }
    end

    test "destroy deletes a principal permission and redirects to the principal" do
      principal = principals(:acme_user_bob)
      delete_permission = principal.slack_channel_permissions.create!(
        channel_id: "C0123456789",
        upload_enabled: true
      )
      keep_permission = principal.slack_channel_permissions.create!(
        channel_id: "G9876543210",
        upload_enabled: true
      )

      assert_difference -> { principal.slack_channel_permissions.count }, -1 do
        delete console_slack_channel_permission_url(delete_permission.oid)
      end

      assert_redirected_to console_principal_path(principal.oid)
      assert_equal "Deleted Slack channel permission.", flash[:notice]
      assert_raises(ActiveRecord::RecordNotFound) { delete_permission.reload }
      assert_predicate keep_permission.reload, :persisted?
    end

    test "destroy deletes a role permission and redirects to the role" do
      role = roles(:acme_infra)
      delete_permission = role.slack_channel_permissions.create!(
        channel_id: "C0123456789",
        upload_enabled: true
      )
      keep_permission = role.slack_channel_permissions.create!(
        channel_id: "G9876543210",
        upload_enabled: true
      )

      assert_difference -> { role.slack_channel_permissions.count }, -1 do
        delete console_slack_channel_permission_url(delete_permission.oid)
      end

      assert_redirected_to console_role_path(role.oid)
      assert_equal "Deleted Slack channel permission.", flash[:notice]
      assert_raises(ActiveRecord::RecordNotFound) { delete_permission.reload }
      assert_predicate keep_permission.reload, :persisted?
    end
  end
end
