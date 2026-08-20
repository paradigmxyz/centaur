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
      @delivery_principal = ConsoleUserPrincipalProvisioner.call(@operator)
      post login_url, params: { email: @operator.email, password: "password123456" }
    end

    test "principal options return bounded catalog matches and exclude existing permissions" do
      principal = principals(:acme_channel)
      existing = principal.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)
      SlackBotChannel.create!(
        team_id: "T0123456789",
        bot_user_id: "U0999999999",
        channel_id: "C1111111111",
        name: "engineering",
        active: true
      )

      with_catalog do
        get console_principal_slack_channel_options_url(principal.oid), params: { q: "eng" }
      end

      assert_response :ok
      assert_equal "no-store", response.headers.fetch("Cache-Control")
      assert_not_equal existing.channel_id, response.parsed_body.dig("options", 0, "value")
      assert_equal(
        {
          "options" => [
            {
              "value" => "C1111111111",
              "label" => "#engineering",
              "description" => "C1111111111 · Public"
            }
          ],
          "error" => nil
        },
        response.parsed_body
      )
    end

    test "role options use the same lookup endpoint" do
      with_catalog do
        get slack_channel_options_console_role_url(roles(:acme_infra).oid), params: { q: "no-match" }
      end

      assert_response :ok
      assert_equal [], response.parsed_body.fetch("options")
    end

    test "scheduled task options include only channels the author can upload to" do
      captured = nil
      @delivery_principal.slack_channel_permissions.create!(
        channel_id: "C1111111111",
        upload_enabled: true
      )
      result = SlackChannelCatalog::Result.new(
        channels: [ SlackChannelCatalog::Channel.new(id: "C1111111111", name: "engineering", private: false) ],
        error: nil,
        configured: true
      )

      search = lambda do |**args|
        captured = args
        result
      end
      SlackChannelCatalogProvider.stub(:search, search) do
        get slack_channel_options_console_scheduled_tasks_url, params: { q: "eng" }
      end

      assert_response :ok
      assert_equal [ "C1111111111" ], captured.fetch(:include_ids)
      assert_equal "C1111111111", response.parsed_body.fetch("options").sole.fetch("value")
    end

    test "scheduled task options include the author's direct message" do
      result = SlackChannelCatalog::Result.new(channels: [], error: nil, configured: true)

      SlackChannelCatalogProvider.stub(:search, result) do
        get slack_channel_options_console_scheduled_tasks_url
      end

      assert_response :ok
      assert_equal(
        {
          "value" => "U0123456789",
          "label" => "Direct message to you",
          "description" => "U0123456789 · Slack DM"
        },
        response.parsed_body.fetch("options").sole
      )
    end

    test "non-admins cannot search the catalog" do
      delete logout_url
      post login_url, params: { email: users(:member_user).email, password: "password123456" }

      get console_principal_slack_channel_options_url(principals(:acme_channel).oid)

      assert_redirected_to console_threads_path
    end

    private

    def with_catalog(&)
      with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-test-token", &)
    end
  end
end
