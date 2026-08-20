require "test_helper"

class Console::ScheduledTasksControllerTest < ActionDispatch::IntegrationTest
  setup do
    @operator = users(:acme_admin)
    @operator.user_identities.create!(
      provider: "slack",
      subject: "U0123456789",
      team_id: "T0123456789"
    )
    post login_url, params: { email: @operator.email, password: "password123456" }
  end

  test "renders the single-turn task form" do
    get new_console_scheduled_task_url

    assert_response :ok
    assert_select "textarea[name='scheduled_task[prompt]']"
    assert_select "select[name='scheduled_task[principal_oid]']", count: 0
    assert_select "select[name='scheduled_task[schedule_preset]']"
    assert_select "select[name='scheduled_task[timezone]']", count: 0
    assert_select "[data-schedule-fields-target=cron][hidden]"
    assert_select "input[name='scheduled_task[cron_expression]'][disabled]"
    assert_select "p", text: "All schedules use Pacific Time."
    assert_select "form[data-controller='slack-channel-autocomplete']"
    assert_select "[data-slack-channel-autocomplete-url-value=?]",
                  slack_channel_options_console_scheduled_tasks_path
    assert_select "[data-slack-channel-autocomplete-dm-user-id-value=U0123456789]"
    assert_select "input[type=radio][name='scheduled_task[delivery_mode]'][value=dm][checked]"
    assert_select "input[type=radio][name='scheduled_task[delivery_mode]'][value=channel]"
    assert_select "[data-slack-channel-autocomplete-target=channelFields][hidden]"
    assert_select "input[type=hidden][name='scheduled_task[delivery_channel]'][data-slack-channel-autocomplete-target=value]"
    assert_select "input[role=combobox][placeholder='Search channels or enter an ID']"
    assert_select "input[type=submit][data-slack-channel-autocomplete-target=submit]:not([disabled])"
    assert_select "a[href=?]", console_scheduled_tasks_path, text: "Cancel"
  end

  test "creates an author-principal task from a schedule preset" do
    assert_difference -> { ScheduledTask.count }, 1 do
      post console_scheduled_tasks_url, params: { scheduled_task: task_params }
    end

    task = ScheduledTask.order(:id).last
    assert_redirected_to console_scheduled_tasks_path
    assert_equal @operator, task.author
    assert_equal @operator, task.execution_principal.console_user
    assert_equal "0 9 * * 1-5", task.cron_expression
    assert_equal ScheduledTask::DEFAULT_TIMEZONE, task.timezone
    assert_not_nil task.next_run_at
  end

  test "creates a task that delivers to the author's Slack DM" do
    post console_scheduled_tasks_url, params: {
      scheduled_task: task_params.merge(delivery_mode: "dm", delivery_channel: "C9999999999")
    }

    assert_redirected_to console_scheduled_tasks_path
    assert_equal "U0123456789", ScheduledTask.order(:id).last.delivery_channel
  end

  test "rejects a delivery channel outside the author's Slack permissions" do
    assert_no_difference -> { ScheduledTask.count } do
      post console_scheduled_tasks_url, params: {
        scheduled_task: task_params.merge(delivery_channel: "C9999999999")
      }
    end

    assert_response :unprocessable_entity
    assert_select "li", text: "Delivery channel is not available to the author"
  end

  test "edit form preserves the selected delivery channel" do
    task = create_task

    get edit_console_scheduled_task_url(task.oid)

    assert_response :ok
    assert_select "input[type=radio][name='scheduled_task[delivery_mode]'][value=channel][checked]"
    assert_select "[data-slack-channel-autocomplete-target=channelFields]:not([hidden])"
    assert_select "input[type=hidden][name='scheduled_task[delivery_channel]'][value=?]", task.delivery_channel
    assert_select "input[role=combobox][value=?]", task.delivery_channel
  end

  test "edit form shows the cron expression for a cron schedule" do
    task = create_task
    task.update!(cron_expression: "15 6 * * *")

    get edit_console_scheduled_task_url(task.oid)

    assert_response :ok
    assert_select "option[value=cron][selected]"
    assert_select "[data-schedule-fields-target=cron]:not([hidden])"
    assert_select "input[name='scheduled_task[cron_expression]'][required]:not([disabled])"
  end

  test "shows scheduled tasks on their dedicated page" do
    task = create_task
    get console_scheduled_tasks_url

    assert_response :ok
    assert_select ".console-thread-group-title-active", text: /Scheduled/
    assert_select "h1", text: "Scheduled Tasks"
    assert_select "a[href=?]", edit_console_scheduled_task_path(task.oid), text: task.name
    assert_select "td", text: /Hourly/
    assert_select "td", text: /#general/
    assert_select "td", text: /#{task.delivery_channel}/
    assert_no_match task.timezone, response.body
  end

  test "shows the author's direct message name on the task table" do
    create_task.update!(delivery_channel: "U0123456789")

    get console_scheduled_tasks_url

    assert_response :ok
    assert_select "td", text: /Direct message to you/
    assert_select "td", text: /U0123456789/
  end

  test "queues a manual run" do
    task = create_task

    assert_enqueued_jobs 1, only: ScheduledTaskRunJob do
      post run_console_scheduled_task_url(task.oid)
    end

    assert_redirected_to console_scheduled_tasks_path
  end

  test "users can only view and edit their own scheduled tasks" do
    other_task = ScheduledTask.create!(
      name: "Other user's task",
      prompt: "Summarize updates.",
      author: users(:globex_admin),
      delivery_channel: "C0123456789",
      cron_expression: "0 * * * *"
    )

    get console_scheduled_tasks_url
    assert_response :ok
    assert_select "a", text: other_task.name, count: 0

    get edit_console_scheduled_task_url(other_task.oid)
    assert_response :not_found
  end

  test "non-admins cannot author tasks" do
    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    assert_no_difference -> { ScheduledTask.count } do
      post console_scheduled_tasks_url, params: { scheduled_task: task_params }
    end

    assert_redirected_to console_threads_path
  end

  private

  def task_params
    {
      name: "Weekday incident summary",
      prompt: "Summarize open incidents.",
      delivery_mode: "channel",
      delivery_channel: "C0123456789",
      schedule_preset: "weekdays",
      cron_expression: "",
      timezone: "America/Denver",
      enabled: "1"
    }
  end

  def create_task
    ScheduledTask.create!(
      name: "Manual run",
      prompt: "Summarize open incidents.",
      author: @operator,
      delivery_channel: "C0123456789",
      cron_expression: "0 * * * *",
      enabled: true
    )
  end
end
