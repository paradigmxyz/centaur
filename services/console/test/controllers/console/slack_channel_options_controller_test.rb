require "test_helper"

module Console
  class SlackChannelOptionsControllerTest < ActionDispatch::IntegrationTest
    setup do
      @operator = users(:acme_admin)
      @operator.user_identities.create!(
        provider: "slack",
        subject: "U0123456789",
        team_id: "T0123456789"
      )
      post login_url, params: { email: @operator.email, password: "password123456" }
    end

    test "role options use the same lookup endpoint" do
      with_catalog do
        get slack_channel_options_console_role_url(roles(:acme_infra).oid), params: { q: "no-match" }
      end

      assert_response :ok
      assert_equal [], response.parsed_body.fetch("options")
    end

    test "non-admins cannot search the catalog" do
      delete logout_url
      post login_url, params: { email: users(:member_user).email, password: "password123456" }

      get slack_channel_options_console_role_url(roles(:acme_infra).oid),
          params: { owner_type: "scheduled_task" }

      assert_redirected_to console_integrations_path
    end

    private

    def with_catalog(&)
      with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-test-token", &)
    end
  end
end
