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

    test "principal options return bounded catalog matches and exclude existing permissions" do
      principal = principals(:acme_channel)
      existing = principal.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)
      SlackBotChannel.create!(
        team_id: "T0123456789",
        bot_user_id: "U0999999999",
        channel_id: "C1111111111",
        name: "engineering",
        active: true,
        member_user_ids: [ "U0999999999" ]
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

    test "scheduled task options include public channels and shared private channels" do
      public_channel = SlackBotChannel.create!(
        team_id: "T0123456789",
        bot_user_id: "U0999999999",
        channel_id: "C1111111111",
        name: "engineering-public",
        active: true,
        member_user_ids: []
      )
      private_channel = SlackBotChannel.create!(
        team_id: "T0123456789",
        bot_user_id: "U0999999999",
        channel_id: "G1111111111",
        name: "engineering-private-shared",
        private: true,
        active: true,
        member_user_ids: [ "U0123456789", "U0999999999" ]
      )
      SlackBotChannel.create!(
        team_id: "T0123456789",
        bot_user_id: "U0999999999",
        channel_id: "G2222222222",
        name: "engineering-private-user-only",
        private: true,
        active: true,
        member_user_ids: [ "U0123456789" ]
      )
      SlackBotChannel.create!(
        team_id: "T0123456789",
        bot_user_id: "U0999999999",
        channel_id: "G3333333333",
        name: "engineering-private-bot-only",
        private: true,
        active: true,
        member_user_ids: [ "U0999999999" ]
      )

      with_catalog do
        get slack_channel_options_console_scheduled_tasks_url, params: { q: "eng" }
      end

      assert_response :ok
      assert_equal [ public_channel.channel_id, private_channel.channel_id ].sort,
                   response.parsed_body.fetch("options").pluck("value").sort
    end

    test "scheduled task channel options do not mix in the author's direct message" do
      with_catalog do
        get slack_channel_options_console_scheduled_tasks_url, params: { q: "dm" }
      end

      assert_response :ok
      assert_empty response.parsed_body.fetch("options")
    end

    test "scheduled task options include public channels without a linked Slack identity" do
      delete logout_url
      user = users(:globex_admin)
      post login_url, params: { email: user.email, password: "password123456" }

      with_catalog do
        get slack_channel_options_console_scheduled_tasks_url, params: { q: "general" }
      end

      assert_response :ok
      assert_nil response.parsed_body["error"]
      assert_equal "C0123456789", response.parsed_body.fetch("options").sole.fetch("value")
    end

    test "scheduled task options use a connected Slack credential identity" do
      delete logout_url
      user = users(:globex_admin)
      app = oauth_apps(:acme_slack)
      app.client_secret = "slack-client-secret"
      app.update!(labels: app.labels.merge("slack_team_id" => "T0123456789"))
      BrokerCredential.create!(
        oauth_app: app,
        provider_subject: "U2222222222",
        provider_email: user.email,
        token_endpoint: app.provider_strategy.token_endpoint,
        refresh_token: "refresh-slack-options",
        access_token: "access-slack-options",
        expires_at: 1.hour.from_now,
        last_refresh: Time.current,
        external_user_key: "user-slack-options",
        created_by: user
      )
      SlackBotChannel.create!(
        team_id: "T0123456789",
        bot_user_id: "U0999999999",
        channel_id: "G2222222222",
        name: "credential-shared",
        private: true,
        active: true,
        member_user_ids: [ "U0999999999", "U2222222222" ],
        membership_refreshed_at: Time.current
      )
      post login_url, params: { email: user.email, password: "password123456" }

      with_catalog do
        get slack_channel_options_console_scheduled_tasks_url, params: { q: "credential" }
      end

      assert_response :ok
      assert_nil response.parsed_body["error"]
      assert_equal "G2222222222", response.parsed_body.fetch("options").sole.fetch("value")
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
