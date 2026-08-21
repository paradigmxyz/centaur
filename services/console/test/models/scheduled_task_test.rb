require "test_helper"

class ScheduledTaskTest < ActiveSupport::TestCase
  setup do
    user = users(:acme_admin)
    user.user_identities.create!(
      provider: "slack",
      subject: "U0123456789",
      team_id: "T0123456789"
    )
  end

  test "maps schedule presets to cron and calculates the next run in Pacific Time" do
    travel_to Time.utc(2026, 8, 19, 12) do
      task = ScheduledTask.create!(
        valid_attributes(
          cron_expression: ScheduledTask.cron_for("daily", nil)
        )
      )

      assert_equal "0 9 * * *", task.cron_expression
      assert_equal ScheduledTask::DEFAULT_TIMEZONE, task.timezone
      assert_equal Time.utc(2026, 8, 19, 16), task.next_run_at
      assert_equal "daily", task.schedule_preset
      assert_equal "Daily at 9:00 PT", task.schedule_label
    end
  end

  test "maps the requested weekday presets to cron schedules" do
    assert_equal "0 9 * * 1-5", ScheduledTask.cron_for("weekdays")
    assert_equal "0 9 * * 1", ScheduledTask.cron_for("mondays")
    assert_equal "0 9 * * 5", ScheduledTask.cron_for("fridays")
  end

  test "builds a custom cron schedule from selected days and time" do
    cron = ScheduledTask.cron_for(
      "custom",
      custom_days: %w[5 1 3 9],
      custom_time: "14:30"
    )
    task = ScheduledTask.new(valid_attributes(cron_expression: cron))

    assert_equal "30 14 * * 1,3,5", cron
    assert_equal "custom", task.schedule_preset
    assert_equal %w[1 3 5], task.custom_schedule_days
    assert_equal "14:30", task.custom_schedule_time
    assert_equal "Mon, Wed, Fri at 2:30 PM PT", task.schedule_label
  end

  test "rejects incomplete custom schedules" do
    assert_empty ScheduledTask.cron_for("custom", custom_days: [], custom_time: "09:00")
    assert_empty ScheduledTask.cron_for("custom", custom_days: %w[1], custom_time: "25:00")
  end

  test "uses the cron expression as the schedule label when it cannot be edited as a weekly schedule" do
    task = ScheduledTask.new(valid_attributes(cron_expression: "0 * * * *"))

    assert_equal "0 * * * *", task.schedule_label
  end

  test "validates custom cron schedules and Slack delivery channels" do
    task = ScheduledTask.new(
      valid_attributes(cron_expression: "not cron", delivery_channel: "general")
    )

    assert_not task.valid?
    assert_includes task.errors[:cron_expression], "is not a valid cron schedule"
    assert_includes task.errors[:delivery_channel], "must be a Slack channel or user ID"
  end

  test "allows the author's direct message and rejects an unavailable channel" do
    direct_message_task = ScheduledTask.new(valid_attributes(delivery_channel: "U0123456789"))
    unavailable_task = ScheduledTask.new(valid_attributes(delivery_channel: "C9999999999"))

    assert direct_message_task.valid?
    assert_not unavailable_task.valid?
    assert_includes unavailable_task.errors[:delivery_channel], "is not available to the author"
  end

  test "disabling a task clears its next run" do
    task = ScheduledTask.create!(valid_attributes)

    task.update!(enabled: false)

    assert_nil task.next_run_at
  end

  test "builds api input with the author's Console principal" do
    task = ScheduledTask.create!(valid_attributes)
    principal = task.execution_principal

    assert_equal "console_user", principal.kind
    assert_equal task.author, principal.console_user
    assert_equal(
      {
        prompt: "Summarize open incidents.",
        principal: principal.foreign_id,
        channel: "C0123456789",
        scheduled_task_id: task.oid,
        scheduled_task_name: "Incident summary"
      },
      task.api_input
    )
  end

  private

  def valid_attributes(overrides = {})
    {
      name: "Incident summary",
      prompt: "Summarize open incidents.",
      author: users(:acme_admin),
      delivery_channel: "C0123456789",
      cron_expression: "0 * * * *",
      enabled: true
    }.merge(overrides)
  end
end
