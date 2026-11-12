require "test_helper"

module Api
  module V1
    # Requests use the console session cookie (+ CSRF) as the SPA does.
    class ConsoleScheduledTasksTest < ActionDispatch::IntegrationTest
      include ActiveJob::TestHelper

      setup do
        @operator = users(:acme_admin)
        @operator.user_identities.create!(provider: "slack", subject: "U0123456789", team_id: "T0123456789")
        sign_in_console(@operator)
      end

      test "scheduled task endpoints are open to active users" do
        task = create_task

        assert_active_user_api :get, api_v1_scheduled_tasks_path
        assert_active_user_api :get, new_api_v1_scheduled_task_path
        assert_active_user_api :get, api_v1_scheduled_task_path(task.oid, view: "console"), credential: :session
        assert_active_user_api :post, api_v1_scheduled_tasks_path, params: { data: task_params }
        assert_active_user_api :patch, api_v1_scheduled_task_path(task.oid), params: { data: { name: "x" } }
        assert_active_user_api :delete, api_v1_scheduled_task_path(task.oid), params: {}
        assert_active_user_api :post, run_api_v1_scheduled_task_path(task.oid), params: {}
        assert_active_user_api :get, slack_channel_options_api_v1_scheduled_tasks_path
        assert task.reload.persisted?
      end

      test "the execution-state lookup keeps its admin-only contract" do
        task = create_task

        assert_admin_only_api :get, api_v1_scheduled_task_path(task.oid)

        get api_v1_scheduled_task_path(task.oid)
        assert_response :ok
        assert_equal({ "id" => task.oid, "enabled" => true, "delivery_channel" => "C0123456789" }, json_body["data"])
      end

      test "new returns the form defaults" do
        get new_api_v1_scheduled_task_path

        assert_response :ok
        assert_equal "no-store", response.headers["Cache-Control"]
        data = json_body["data"]
        assert_nil data["id"]
        assert_equal "daily", data["schedule_preset"]
        assert_equal "0 9 * * *", data["cron_expression"]
        assert_equal true, data["enabled"]
        assert_equal "dm", data["delivery_mode"]
        assert_equal "U0123456789", data["direct_message_user_id"]
        assert_equal "09:00", data["custom_time"]
      end

      test "new defaults to channel delivery without a linked Slack identity" do
        sign_in_console(users(:member_user))

        get new_api_v1_scheduled_task_path

        assert_equal "channel", json_body.dig("data", "delivery_mode")
        assert_nil json_body.dig("data", "direct_message_user_id")
      end

      test "creates an author-principal task from a schedule preset" do
        assert_difference -> { ScheduledTask.count }, 1 do
          post api_v1_scheduled_tasks_path, params: { data: task_params }, headers: csrf_headers, as: :json
        end

        assert_response :created
        task = ScheduledTask.order(:id).last
        assert_equal task.oid, json_body.dig("data", "id")
        assert_equal @operator, task.author
        assert_equal @operator, task.execution_principal.console_user
        assert_equal "0 9 * * 1-5", task.cron_expression
        assert_equal ScheduledTask::DEFAULT_TIMEZONE, task.timezone
        assert_not_nil task.next_run_at
        assert_equal "weekdays", json_body.dig("data", "schedule_preset")
        assert_equal "Weekdays at 9:00 PT", json_body.dig("data", "schedule_label")
      end

      test "creates an ad-hoc task without a schedule" do
        post api_v1_scheduled_tasks_path, params: { data: task_params.merge(schedule_preset: "manual") },
                                          headers: csrf_headers, as: :json

        assert_response :created
        task = ScheduledTask.order(:id).last
        assert_nil task.cron_expression
        assert_nil task.next_run_at
        assert_equal "Manual only", json_body.dig("data", "schedule_label")
      end

      test "creates a task that delivers to the author's Slack DM" do
        post api_v1_scheduled_tasks_path,
             params: { data: task_params.merge(delivery_mode: "dm", delivery_channel: "C9999999999") },
             headers: csrf_headers, as: :json

        assert_response :created
        assert_equal "U0123456789", ScheduledTask.order(:id).last.delivery_channel
        assert_equal "dm", json_body.dig("data", "delivery_mode")
        assert_equal "Direct message to you", json_body.dig("data", "delivery_name")
      end

      test "rejects a delivery channel outside the author's Slack permissions" do
        assert_no_difference -> { ScheduledTask.count } do
          post api_v1_scheduled_tasks_path, params: { data: task_params.merge(delivery_channel: "C9999999999") },
                                            headers: csrf_headers, as: :json
        end

        assert_response :unprocessable_entity
        assert_equal [ "is not available to the author" ], json_body.dig("error", "details", "delivery_channel")
      end

      test "rejects a DM delivery without a linked Slack identity" do
        sign_in_console(users(:member_user))

        post api_v1_scheduled_tasks_path, params: { data: task_params.merge(delivery_mode: "dm") },
                                          headers: csrf_headers, as: :json

        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "delivery_channel").present?
      end

      test "the console view preserves the selected delivery channel" do
        task = create_task

        get api_v1_scheduled_task_path(task.oid, view: "console")

        assert_response :ok
        data = json_body["data"]
        assert_equal "channel", data["delivery_mode"]
        assert_equal "C0123456789", data["delivery_channel"]
        assert_equal "#general", data["delivery_name"]
        assert_equal task.prompt, data["prompt"]
      end

      test "the console view maps a weekly cron schedule to custom day and time fields" do
        task = create_task
        task.update!(cron_expression: "15 6 * * 1,3,5")

        get api_v1_scheduled_task_path(task.oid, view: "console")

        data = json_body["data"]
        assert_equal "custom", data["schedule_preset"]
        assert_equal %w[1 3 5], data["custom_days"]
        assert_equal "06:15", data["custom_time"]
      end

      test "creates a task from custom weekdays and time" do
        assert_difference -> { ScheduledTask.count }, 1 do
          post api_v1_scheduled_tasks_path,
               params: { data: task_params.merge(schedule_preset: "custom", custom_days: %w[1 3 5], custom_time: "14:30") },
               headers: csrf_headers, as: :json
        end

        task = ScheduledTask.order(:id).last
        assert_equal "30 14 * * 1,3,5", task.cron_expression
        assert_equal "Mon, Wed, Fri at 2:30 PM PT", json_body.dig("data", "schedule_label")
      end

      test "rejects a custom schedule without any days" do
        assert_no_difference -> { ScheduledTask.count } do
          post api_v1_scheduled_tasks_path,
               params: { data: task_params.merge(schedule_preset: "custom", custom_days: [], custom_time: "14:30") },
               headers: csrf_headers, as: :json
        end

        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "cron_expression").present?
      end

      test "lists the caller's tasks with schedule and delivery names" do
        task = create_task
        get api_v1_scheduled_tasks_path

        assert_response :ok
        assert_equal "no-store", response.headers["Cache-Control"]
        row = json_body["data"].sole
        assert_equal task.oid, row["id"]
        assert_equal task.name, row["name"]
        assert_equal "0 * * * *", row["schedule_label"]
        assert_equal "#general", row["delivery_name"]
        assert_equal task.delivery_channel, row["delivery_channel"]
      end

      test "shows the author's direct message name on the task list" do
        create_task.update!(delivery_channel: "U0123456789")

        get api_v1_scheduled_tasks_path

        row = json_body["data"].sole
        assert_equal "Direct message to you", row["delivery_name"]
        assert_equal "U0123456789", row["delivery_channel"]
      end

      test "updates a task and keeps fields that are not sent" do
        task = create_task

        patch api_v1_scheduled_task_path(task.oid), params: { data: { name: "Renamed" } },
                                                    headers: csrf_headers, as: :json

        assert_response :ok
        task.reload
        assert_equal "Renamed", task.name
        assert_equal "0 * * * *", task.cron_expression
        assert_equal "C0123456789", task.delivery_channel
      end

      test "queues a manual run" do
        task = create_task

        assert_enqueued_jobs 1, only: ScheduledTaskRunJob do
          post run_api_v1_scheduled_task_path(task.oid), headers: csrf_headers, as: :json
        end

        assert_response :accepted
        assert_equal true, json_body.dig("data", "queued")
      end

      test "does not allow a disabled task to run" do
        task = create_task
        task.update!(enabled: false)

        get api_v1_scheduled_tasks_path
        assert_equal false, json_body["data"].sole["enabled"]

        assert_no_enqueued_jobs only: ScheduledTaskRunJob do
          post run_api_v1_scheduled_task_path(task.oid), headers: csrf_headers, as: :json
        end
        assert_response :unprocessable_entity
        assert_equal "Enable the task before running it.", json_body.dig("error", "message")
        assert_equal "task_disabled", json_body.dig("error", "details", "reason")
      end

      test "users can only view and edit their own scheduled tasks" do
        other_task = create_task(author: @operator, name: "Admin's task")
        member = sign_in_console(users(:member_user))
        own = create_task(author: member, name: "Member task")

        get api_v1_scheduled_tasks_path
        assert_equal [ own.oid ], json_body["data"].pluck("id")

        get api_v1_scheduled_task_path(other_task.oid, view: "console")
        assert_response :not_found

        patch api_v1_scheduled_task_path(other_task.oid), params: { data: { name: "Hijacked" } },
                                                          headers: csrf_headers, as: :json
        assert_response :not_found

        assert_no_enqueued_jobs only: ScheduledTaskRunJob do
          post run_api_v1_scheduled_task_path(other_task.oid), headers: csrf_headers, as: :json
        end
        assert_response :not_found

        delete api_v1_scheduled_task_path(other_task.oid), headers: csrf_headers, as: :json
        assert_response :not_found

        get slack_channel_options_api_v1_scheduled_tasks_path(task_id: other_task.oid)
        assert_response :not_found

        assert_equal "Admin's task", other_task.reload.name
      end

      test "admins get 404 on another user's task for every member action" do
        member = users(:member_user)
        task = create_task(author: member, name: "Member task")
        own = create_task

        get api_v1_scheduled_tasks_path
        assert_equal [ own.oid ], json_body["data"].pluck("id")

        get api_v1_scheduled_tasks_path(scope: "all")
        assert_equal [ own.oid ], json_body["data"].pluck("id")
        assert_not json_body["data"].first.key?("author_email")

        get api_v1_scheduled_task_path(task.oid, view: "console")
        assert_response :not_found

        patch api_v1_scheduled_task_path(task.oid), params: { data: { name: "Renamed by admin" } },
                                                    headers: csrf_headers, as: :json
        assert_response :not_found

        assert_no_enqueued_jobs only: ScheduledTaskRunJob do
          post run_api_v1_scheduled_task_path(task.oid), headers: csrf_headers, as: :json
        end
        assert_response :not_found

        assert_no_difference -> { ScheduledTask.count } do
          delete api_v1_scheduled_task_path(task.oid), headers: csrf_headers, as: :json
        end
        assert_response :not_found

        get slack_channel_options_api_v1_scheduled_tasks_path(task_id: task.oid)
        assert_response :not_found

        assert_equal "Member task", task.reload.name

        # The admin-only execution-state lookup still reaches any task.
        get api_v1_scheduled_task_path(task.oid)
        assert_response :ok
        assert_equal({ "id" => task.oid, "enabled" => true, "delivery_channel" => "C0123456789" }, json_body["data"])
      end

      test "Slack channel tasks are not reachable from the console endpoints" do
        principal = principals(:acme_channel)
        principal.update!(slack_team_id: "T0123456789")
        task = ScheduledTask.create!(name: "Channel task", prompt: "Hi", principal: principal,
                                     delivery_channel: "C0123456789")

        get api_v1_scheduled_tasks_path
        assert_not_includes json_body["data"].pluck("id"), task.oid

        get api_v1_scheduled_task_path(task.oid, view: "console")
        assert_response :not_found
      end

      test "non-admins can create and manage their own tasks" do
        member = sign_in_console(users(:member_user))

        get new_api_v1_scheduled_task_path
        assert_response :ok

        with_catalog do
          assert_difference -> { ScheduledTask.count }, 1 do
            post api_v1_scheduled_tasks_path, params: { data: task_params }, headers: csrf_headers, as: :json
          end
        end
        assert_response :created
        task = member.scheduled_tasks.find_by!(name: task_params.fetch(:name))

        patch api_v1_scheduled_task_path(task.oid), params: { data: task_params.merge(name: "Updated member task") },
                                                    headers: csrf_headers, as: :json
        assert_response :ok
        assert_equal "Updated member task", task.reload.name

        assert_enqueued_jobs 1, only: ScheduledTaskRunJob do
          post run_api_v1_scheduled_task_path(task.oid), headers: csrf_headers, as: :json
        end
        assert_response :accepted

        assert_difference -> { ScheduledTask.count }, -1 do
          delete api_v1_scheduled_task_path(task.oid), headers: csrf_headers, as: :json
        end
        assert_response :no_content
      end

      test "delivery options include public channels and shared private channels" do
        public_channel = create_channel("C1111111111", "engineering-public", members: [])
        private_channel = create_channel("G1111111111", "engineering-private-shared", private: true,
                                                                                    members: %w[U0123456789 U0999999999])
        create_channel("G2222222222", "engineering-private-user-only", private: true, members: %w[U0123456789])
        create_channel("G3333333333", "engineering-private-bot-only", private: true, members: %w[U0999999999])

        with_catalog do
          get slack_channel_options_api_v1_scheduled_tasks_path, params: { q: "eng" }
        end

        assert_response :ok
        assert_equal "no-store", response.headers["Cache-Control"]
        assert_equal [ public_channel.channel_id, private_channel.channel_id ].sort,
                     json_body["data"].pluck("channel_id").sort
        option = json_body["data"].find { |row| row["channel_id"] == private_channel.channel_id }
        assert_equal "#engineering-private-shared", option["label"]
        assert_equal "G1111111111 · Private", option["description"]
      end

      test "delivery options do not mix in the author's direct message" do
        with_catalog do
          get slack_channel_options_api_v1_scheduled_tasks_path, params: { q: "dm" }
        end

        assert_response :ok
        assert_empty json_body["data"]
      end

      test "members without a linked Slack identity get public delivery channels" do
        sign_in_console(users(:member_user))

        with_catalog do
          get slack_channel_options_api_v1_scheduled_tasks_path, params: { q: "general" }
        end

        assert_response :ok
        assert_nil json_body.dig("meta", "catalog_error")
        assert_equal "C0123456789", json_body["data"].sole["channel_id"]
      end

      test "delivery options use a connected Slack credential identity" do
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
        create_channel("G2222222222", "credential-shared", private: true, members: %w[U0999999999 U2222222222])
        sign_in_console(user)

        with_catalog do
          get slack_channel_options_api_v1_scheduled_tasks_path, params: { q: "credential" }
        end

        assert_response :ok
        assert_nil json_body.dig("meta", "catalog_error")
        assert_equal "G2222222222", json_body["data"].sole["channel_id"]
      end

      test "delivery options accept the caller's own task id" do
        task = create_task
        shared = create_channel("G1111111111", "eng-shared", private: true, members: %w[U0123456789 U0999999999])

        with_catalog do
          get slack_channel_options_api_v1_scheduled_tasks_path, params: { q: "eng", task_id: task.oid }
        end

        assert_response :ok
        assert_includes json_body["data"].pluck("channel_id"), shared.channel_id
      end

      test "delivery options report an unconfigured catalog" do
        with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => nil) do
          get slack_channel_options_api_v1_scheduled_tasks_path
        end

        assert_response :ok
        assert_equal "SLACK_BOT_TOKEN is not configured.", json_body.dig("meta", "catalog_error")
      end

      private

      def task_params
        {
          name: "Weekday incident summary",
          prompt: "Summarize open incidents.",
          delivery_mode: "channel",
          delivery_channel: "C0123456789",
          schedule_preset: "weekdays",
          enabled: true
        }
      end

      def create_task(author: @operator, name: "Manual run")
        ScheduledTask.create!(
          name: name,
          prompt: "Summarize open incidents.",
          author: author,
          delivery_channel: "C0123456789",
          cron_expression: "0 * * * *",
          enabled: true
        )
      end

      def create_channel(channel_id, name, members:, private: false)
        SlackBotChannel.create!(team_id: "T0123456789", bot_user_id: "U0999999999", channel_id: channel_id,
                                name: name, private: private, active: true, member_user_ids: members,
                                membership_refreshed_at: Time.current)
      end

      def with_catalog(&)
        with_env("CENTAUR_CONSOLE_SLACK_BOT_TOKEN" => "xoxb-test-token", &)
      end
    end
  end
end
