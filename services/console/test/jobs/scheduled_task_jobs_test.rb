require "test_helper"

class ScheduledTaskJobsTest < ActiveJob::TestCase
  class FakeApiClient
    attr_reader :requests

    def initialize
      @requests = []
    end

    def create_workflow_run(**request)
      @requests << request
      { "run_id" => "run-123", "created" => true }
    end
  end

  setup do
    @original_client_factory = ScheduledTaskRunJob.client_factory
    user = users(:acme_admin)
    user.user_identities.create!(
      provider: "slack",
      subject: "U0123456789",
      team_id: "T0123456789"
    )
  end

  teardown do
    ScheduledTaskRunJob.client_factory = @original_client_factory
  end

  test "scheduler claims a due task once and advances its next run" do
    task = create_task
    scheduled_for = Time.utc(2026, 8, 19, 12)
    task.update_columns(next_run_at: scheduled_for)

    assert_enqueued_with(job: ScheduledTaskRunJob, args: [ task.id, scheduled_for.iso8601 ]) do
      ScheduledTaskSchedulerJob.perform_now(scheduled_for)
    end

    task.reload
    assert_equal scheduled_for, task.last_enqueued_at
    assert_equal Time.utc(2026, 8, 19, 13), task.next_run_at

    assert_no_enqueued_jobs only: ScheduledTaskRunJob do
      ScheduledTaskSchedulerJob.perform_now(scheduled_for)
    end
  end

  test "scheduler leaves a task due when the run job cannot be enqueued" do
    task = create_task
    scheduled_for = Time.utc(2026, 8, 19, 12)
    task.update_columns(next_run_at: scheduled_for)

    ScheduledTaskRunJob.stub(:perform_later, false) do
      assert_raises(ActiveJob::EnqueueError) do
        ScheduledTaskSchedulerJob.perform_now(scheduled_for)
      end
    end

    task.reload
    assert_equal scheduled_for, task.next_run_at
    assert_nil task.last_enqueued_at
  end

  test "runner sends the scheduled task input and a stable idempotency key" do
    task = create_task
    client = FakeApiClient.new
    ScheduledTaskRunJob.client_factory = -> { client }

    travel_to Time.utc(2026, 8, 19, 12, 5) do
      ScheduledTaskRunJob.perform_now(task.id, "2026-08-19T12:00:00Z")
    end

    request = client.requests.fetch(0)
    assert_equal "console_workflow", request[:workflow_name]
    assert_equal task.execution_principal.foreign_id, request.dig(:input, :principal)
    assert_equal users(:acme_admin), task.execution_principal.console_user
    assert_equal "Summarize open incidents.", request.dig(:input, :prompt)
    assert_equal "C0123456789", request.dig(:input, :channel)
    assert_equal "scheduled-task:#{task.id}:2026-08-19T12:00:00Z", request[:idempotency_key]
    assert_equal ScheduledTaskRunJob::MAX_ATTEMPTS, request[:max_attempts]
    assert_equal "run-123", task.reload.last_run_id
    assert_equal Time.utc(2026, 8, 19, 12, 5), task.last_run_at
  end

  test "runner refuses a private destination after the author or bot leaves the channel" do
    channel = SlackBotChannel.create!(
      team_id: "T0123456789",
      bot_user_id: "U0999999999",
      channel_id: "G1111111111",
      name: "private-shared",
      private: true,
      active: true,
      member_user_ids: [ "U0123456789", "U0999999999" ]
    )
    task = create_task(delivery_channel: channel.channel_id)
    client = FakeApiClient.new
    ScheduledTaskRunJob.client_factory = -> { client }
    channel.update!(member_user_ids: [ "U0999999999" ])

    assert_raises(ScheduledTask::DeliveryDestinationUnavailable) do
      ScheduledTaskRunJob.perform_now(task.id, "2026-08-19T12:00:00Z")
    end
    assert_empty client.requests
  end

  private

  def create_task(delivery_channel: "C0123456789")
    ScheduledTask.create!(
      name: "Incident summary #{SecureRandom.hex(4)}",
      prompt: "Summarize open incidents.",
      author: users(:acme_admin),
      delivery_channel: delivery_channel,
      cron_expression: "0 * * * *",
      enabled: true
    )
  end
end
