require "test_helper"

class AuthoredWorkflowTest < ActiveSupport::TestCase
  test "maps schedule presets to cron and calculates the next run in its timezone" do
    travel_to Time.utc(2026, 8, 19, 12) do
      workflow = AuthoredWorkflow.create!(
        valid_attributes(
          cron_expression: AuthoredWorkflow.cron_for("daily", nil),
          timezone: "America/Denver"
        )
      )

      assert_equal "0 9 * * *", workflow.cron_expression
      assert_equal Time.utc(2026, 8, 19, 15), workflow.next_run_at
      assert_equal "daily", workflow.schedule_preset
    end
  end

  test "validates custom cron schedules and Slack delivery channels" do
    workflow = AuthoredWorkflow.new(
      valid_attributes(cron_expression: "not cron", delivery_channel: "general")
    )

    assert_not workflow.valid?
    assert_includes workflow.errors[:cron_expression], "is not a valid cron schedule"
    assert_includes workflow.errors[:delivery_channel], "must be a Slack channel ID"
  end

  test "disabling a workflow clears its next run" do
    workflow = AuthoredWorkflow.create!(valid_attributes)

    workflow.update!(enabled: false)

    assert_nil workflow.next_run_at
  end

  test "builds api input with an explicitly selected principal" do
    principal = principals(:acme_channel)
    workflow = AuthoredWorkflow.create!(valid_attributes(principal: principal))

    assert_equal(
      {
        prompt: "Summarize open incidents.",
        principal: principal.foreign_id,
        channel: "C0123456789",
        authored_workflow_id: workflow.oid,
        authored_workflow_name: "Incident summary"
      },
      workflow.api_input
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
      timezone: "UTC",
      enabled: true
    }.merge(overrides)
  end
end
