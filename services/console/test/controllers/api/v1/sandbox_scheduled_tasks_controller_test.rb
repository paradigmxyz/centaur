require "test_helper"

class Api::V1::SandboxScheduledTasksControllerTest < ActionDispatch::IntegrationTest
  setup do
    @user = users(:member_user)
    @user.user_identities.create!(
      provider: "slack",
      subject: "U0123456789",
      team_id: "T0123456789"
    )
    principal = Principal.create!(
      foreign_id: "scheduled-task-user",
      name: "Scheduled Task User",
      kind: :console_user,
      console_user: @user,
      labels: {},
      created_by: @user
    )
    @proxy = Proxy.create!(
      name: "scheduled-task-proxy",
      principal: principal,
      bearer_token_hash: Digest::SHA256.hexdigest("iprx_#{'e' * 64}")
    )
  end

  test "linked user creates reads updates and deletes a scheduled task" do
    assert_difference("ScheduledTask.count", 1) do
      with_token(@proxy) do |headers|
        post "/api/v1/sandbox/scheduled_tasks",
             params: {
               data: {
                 name: "Daily briefing",
                 prompt: "Summarize the important updates.",
                 delivery_channel: "dm",
                 cron_expression: "0 9 * * *",
                 enabled: true
               }
             },
             headers: headers,
             as: :json
      end
    end
    assert_response :created

    task = @user.scheduled_tasks.find_by!(name: "Daily briefing")
    assert_equal "U0123456789", task.delivery_channel
    assert_equal task.oid, json_body.dig("data", "id")
    assert_equal ScheduledTask::DEFAULT_TIMEZONE, json_body.dig("data", "timezone")
    assert_equal "Daily at 9:00 PT", json_body.dig("data", "schedule_label")
    assert_not_nil json_body.dig("data", "next_run_at")

    with_token(@proxy) do |headers|
      get "/api/v1/sandbox/scheduled_tasks/#{task.oid}", headers: headers
    end
    assert_response :ok
    assert_equal "Summarize the important updates.", json_body.dig("data", "prompt")

    with_token(@proxy) do |headers|
      patch "/api/v1/sandbox/scheduled_tasks/#{task.oid}",
            params: { data: { name: "Morning briefing", enabled: false } },
            headers: headers,
            as: :json
    end
    assert_response :ok
    assert_equal "Morning briefing", task.reload.name
    assert_not task.enabled?
    assert_nil task.next_run_at

    assert_difference("ScheduledTask.count", -1) do
      with_token(@proxy) do |headers|
        delete "/api/v1/sandbox/scheduled_tasks/#{task.oid}", headers: headers
      end
    end
    assert_response :no_content
  end

  test "linked user creates an ad-hoc task without a schedule" do
    with_token(@proxy) do |headers|
      post "/api/v1/sandbox/scheduled_tasks",
           params: {
             data: {
               name: "One-off briefing",
               prompt: "Summarize the important updates."
             }
           },
           headers: headers,
           as: :json
    end

    assert_response :created
    assert_equal "U0123456789", json_body.dig("data", "delivery_channel")
    assert_nil json_body.dig("data", "cron_expression")
    assert_nil json_body.dig("data", "next_run_at")
    assert_equal "Manual only", json_body.dig("data", "schedule_label")
  end

  test "linked user converts a scheduled task to manual-only with null cron" do
    task = create_task(author: @user, name: "Make manual")

    with_token(@proxy) do |headers|
      patch "/api/v1/sandbox/scheduled_tasks/#{task.oid}",
            params: { data: { cron_expression: nil } },
            headers: headers,
            as: :json
    end

    assert_response :ok
    assert_nil task.reload.cron_expression
    assert_nil task.next_run_at
    assert_equal "Manual only", json_body.dig("data", "schedule_label")
  end

  test "list and lookup are limited to the linked user's tasks" do
    own_task = create_task(author: @user, name: "My task")
    other_task = create_task(author: users(:globex_admin), name: "Other task")

    with_token(@proxy) do |headers|
      get "/api/v1/sandbox/scheduled_tasks", headers: headers
    end
    assert_response :ok
    assert_equal "no-store", response.headers["Cache-Control"]
    assert_equal [ own_task.oid ], json_body.fetch("data").map { |task| task.fetch("id") }

    with_token(@proxy) do |headers|
      get "/api/v1/sandbox/scheduled_tasks/#{other_task.oid}", headers: headers
    end
    assert_response :not_found
  end

  test "empty cron input creates a manual-only task" do
    assert_difference("ScheduledTask.count", 1) do
      with_token(@proxy) do |headers|
        post "/api/v1/sandbox/scheduled_tasks",
             params: {
               data: {
                 name: "Manual task",
                 prompt: "Do something.",
                 delivery_channel: "dm",
                 cron_expression: ""
               }
             },
             headers: headers,
             as: :json
      end
    end

    assert_response :created
    assert_nil json_body.dig("data", "cron_expression")
    assert_nil json_body.dig("data", "next_run_at")
    assert_equal "Manual only", json_body.dig("data", "schedule_label")
  end

  test "invalid task attributes return validation details" do
    assert_no_difference("ScheduledTask.count") do
      with_token(@proxy) do |headers|
        post "/api/v1/sandbox/scheduled_tasks",
             params: {
               data: {
                 name: "Broken task",
                 prompt: "Do something.",
                 delivery_channel: "not-a-channel",
                 cron_expression: "not cron"
               }
             },
             headers: headers,
             as: :json
      end
    end
    assert_response :unprocessable_entity
    assert_equal "validation failed", json_body.dig("error", "message")
    assert_includes json_body.dig("error", "details", "cron_expression"), "is not a valid cron schedule"
  end

  test "linked user queues an immediate run" do
    task = create_task(author: @user, name: "Run me")

    assert_enqueued_jobs 1, only: ScheduledTaskRunJob do
      with_token(@proxy) do |headers|
        post "/api/v1/sandbox/scheduled_tasks/#{task.oid}/run", headers: headers
      end
    end
    assert_response :accepted
    assert_equal true, json_body.dig("data", "queued")
  end

  test "disabled tasks cannot be run immediately" do
    task = create_task(author: @user, name: "Disabled task")
    task.update!(enabled: false)

    assert_no_enqueued_jobs only: ScheduledTaskRunJob do
      with_token(@proxy) do |headers|
        post "/api/v1/sandbox/scheduled_tasks/#{task.oid}/run", headers: headers
      end
    end

    assert_response :unprocessable_entity
    assert_equal "scheduled task is disabled", json_body.dig("error", "message")
  end

  test "run reports when the task could not be queued" do
    task = create_task(author: @user, name: "Do not run")

    ScheduledTaskRunJob.stub(:perform_later, false) do
      with_token(@proxy) do |headers|
        post "/api/v1/sandbox/scheduled_tasks/#{task.oid}/run", headers: headers
      end
    end

    assert_response :service_unavailable
    assert_equal "scheduled task run could not be queued", json_body.dig("error", "message")
  end

  test "principal without an active linked user cannot manage tasks" do
    with_token(proxies(:globex_proxy)) do |headers|
      get "/api/v1/sandbox/scheduled_tasks", headers: headers
    end
    assert_response :forbidden
  end

  test "anyone in a channel thread manages that channel's tasks" do
    channel = principals(:acme_channel)
    channel_proxy = proxies(:acme_proxy)
    channel_proxy.update!(requester_principal: slack_user_principal("U0123456789"))
    user_task = create_task(author: @user, name: "Personal task")

    with_token(channel_proxy) do |headers|
      post "/api/v1/sandbox/scheduled_tasks",
           params: {
             data: {
               name: "Channel digest",
               prompt: "Summarize this channel.",
               cron_expression: "0 9 * * 1"
             }
           },
           headers: headers,
           as: :json
    end
    assert_response :created
    task = channel.scheduled_tasks.find_by!(name: "Channel digest")
    assert_nil task.author
    assert_equal channel.slack_channel_id, task.delivery_channel

    channel_proxy.update!(requester_principal: slack_user_principal("U2222222222"))
    with_token(channel_proxy) do |headers|
      get "/api/v1/sandbox/scheduled_tasks", headers: headers
      assert_response :ok
      assert_equal [ task.oid ], json_body.fetch("data").map { |row| row.fetch("id") }

      get "/api/v1/sandbox/scheduled_tasks/#{user_task.oid}", headers: headers
      assert_response :not_found

      patch "/api/v1/sandbox/scheduled_tasks/#{task.oid}",
            params: { data: { prompt: "Summarize this channel briefly." } },
            headers: headers,
            as: :json
      assert_response :ok

      assert_enqueued_jobs 1, only: ScheduledTaskRunJob do
        post "/api/v1/sandbox/scheduled_tasks/#{task.oid}/run", headers: headers
      end
      assert_response :accepted

      delete "/api/v1/sandbox/scheduled_tasks/#{task.oid}", headers: headers
      assert_response :no_content
    end
    assert_not ScheduledTask.exists?(task.id)
  end

  test "channel tasks refuse a different delivery destination" do
    channel_proxy = proxies(:acme_proxy)
    channel_proxy.update!(requester_principal: slack_user_principal("U0123456789"))

    assert_no_difference("ScheduledTask.count") do
      with_token(channel_proxy) do |headers|
        post "/api/v1/sandbox/scheduled_tasks",
             params: { data: { name: "DM me", prompt: "Summarize this channel.", delivery_channel: "dm" } },
             headers: headers,
             as: :json
      end
    end

    assert_response :unprocessable_entity
    assert_equal "tasks created in a Slack channel thread always deliver to that channel",
                 json_body.dig("error", "message")
  end

  test "Slack users outside the bot's workspaces cannot manage channel tasks" do
    channel_proxy = proxies(:acme_proxy)
    channel_proxy.update!(requester_principal: slack_user_principal("U0123456789", team_id: "T0EXTERNAL1"))

    with_token(channel_proxy) do |headers|
      get "/api/v1/sandbox/scheduled_tasks", headers: headers
    end

    assert_response :forbidden
  end

  test "channel session without a Slack requester cannot manage channel tasks" do
    with_token(proxies(:acme_proxy)) do |headers|
      post "/api/v1/sandbox/scheduled_tasks",
           params: { data: { name: "Recursive", prompt: "Schedule more work." } },
           headers: headers,
           as: :json
    end

    assert_response :forbidden
    assert_empty principals(:acme_channel).scheduled_tasks
  end

  test "disabled linked user cannot manage tasks" do
    @user.update!(status: :disabled)

    with_token(@proxy) do |headers|
      get "/api/v1/sandbox/scheduled_tasks", headers: headers
    end

    assert_response :forbidden
  end

  private

  def create_task(author:, name:)
    ScheduledTask.create!(
      name: name,
      prompt: "Summarize updates.",
      author: author,
      delivery_channel: "C0123456789",
      cron_expression: "0 * * * *"
    )
  end

  def slack_user_principal(slack_user_id, team_id: "T0123456789")
    Principal.create!(
      foreign_id: "slack-user-#{team_id.downcase}-#{slack_user_id.downcase}",
      name: "Slack User #{slack_user_id}",
      kind: :slack_dm,
      slack_user_id: slack_user_id,
      slack_team_id: team_id,
      labels: {},
      created_by: @user
    )
  end

  def with_token(proxy)
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      yield "Authorization" => "Bearer #{SandboxEntitlements::Jwt.encode_for_proxy(proxy)}"
    end
  end

  def json_body
    JSON.parse(response.body)
  end
end
