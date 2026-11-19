require "test_helper"

# api-rs runs scheduled tasks; the run job only drains runs Console enqueued
# before that.
class ScheduledTaskJobsTest < ActiveJob::TestCase
  setup do
    @task = ScheduledTask.create!(
      name: "Incident summary",
      prompt: "Summarize open incidents.",
      author: users(:acme_admin),
      delivery_channel: "C0123456789",
      cron_expression: "0 * * * *"
    )
    users(:acme_admin).user_identities.create!(provider: "slack", subject: "U0123456789", team_id: "T0123456789")
  end

  test "an enqueued run starts in api-rs for the occurrence it was enqueued for" do
    with_scheduled_task_client do |client|
      ScheduledTaskRunJob.perform_now(@task.id, "2026-08-19T12:00:00Z")

      assert_equal [ { task_oid: @task.oid, scheduled_for: Time.utc(2026, 8, 19, 12) } ], client.runs
    end
  end

  test "a task disabled after its run was enqueued does not run" do
    @task.update!(enabled: false)

    assert_scheduled_task_runs 0 do
      ScheduledTaskRunJob.perform_now(@task.id, "2026-08-19T12:00:00Z")
    end
  end

  test "a run api-rs refuses is not retried" do
    refused = CentaurApiClient::Error.new("Slack delivery destination is no longer available", status: 422)

    with_scheduled_task_client(ScheduledTaskRunHelper::FakeClient.new(error: refused)) do
      assert_no_enqueued_jobs only: ScheduledTaskRunJob do
        ScheduledTaskRunJob.perform_now(@task.id, "2026-08-19T12:00:00Z")
      end
    end
  end

  test "a run api-rs could not start is retried" do
    unavailable = CentaurApiClient::Error.new("database unavailable", status: 503)

    with_scheduled_task_client(ScheduledTaskRunHelper::FakeClient.new(error: unavailable)) do
      assert_enqueued_jobs 1, only: ScheduledTaskRunJob do
        ScheduledTaskRunJob.perform_now(@task.id, "2026-08-19T12:00:00Z")
      end
    end
  end
end
