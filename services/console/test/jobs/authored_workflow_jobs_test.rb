require "test_helper"

class AuthoredWorkflowJobsTest < ActiveJob::TestCase
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
    @original_client_factory = AuthoredWorkflowRunJob.client_factory
  end

  teardown do
    AuthoredWorkflowRunJob.client_factory = @original_client_factory
  end

  test "scheduler claims a due workflow once and advances its next run" do
    workflow = create_workflow
    scheduled_for = Time.utc(2026, 8, 19, 12)
    workflow.update_columns(next_run_at: scheduled_for)

    assert_enqueued_with(job: AuthoredWorkflowRunJob, args: [ workflow.id, scheduled_for.iso8601 ]) do
      AuthoredWorkflowSchedulerJob.perform_now(scheduled_for)
    end

    workflow.reload
    assert_equal scheduled_for, workflow.last_enqueued_at
    assert_equal Time.utc(2026, 8, 19, 13), workflow.next_run_at

    assert_no_enqueued_jobs only: AuthoredWorkflowRunJob do
      AuthoredWorkflowSchedulerJob.perform_now(scheduled_for)
    end
  end

  test "scheduler leaves a workflow due when the run job cannot be enqueued" do
    workflow = create_workflow
    scheduled_for = Time.utc(2026, 8, 19, 12)
    workflow.update_columns(next_run_at: scheduled_for)

    AuthoredWorkflowRunJob.stub(:perform_later, false) do
      assert_raises(ActiveJob::EnqueueError) do
        AuthoredWorkflowSchedulerJob.perform_now(scheduled_for)
      end
    end

    workflow.reload
    assert_equal scheduled_for, workflow.next_run_at
    assert_nil workflow.last_enqueued_at
  end

  test "runner sends the console workflow input and a stable idempotency key" do
    principal = principals(:acme_channel)
    workflow = create_workflow(principal: principal)
    client = FakeApiClient.new
    AuthoredWorkflowRunJob.client_factory = -> { client }

    travel_to Time.utc(2026, 8, 19, 12, 5) do
      AuthoredWorkflowRunJob.perform_now(workflow.id, "2026-08-19T12:00:00Z")
    end

    request = client.requests.fetch(0)
    assert_equal "console_workflow", request[:workflow_name]
    assert_equal principal.foreign_id, request.dig(:input, :principal)
    assert_equal "Summarize open incidents.", request.dig(:input, :prompt)
    assert_equal "C0123456789", request.dig(:input, :channel)
    assert_equal "authored-workflow:#{workflow.id}:2026-08-19T12:00:00Z", request[:idempotency_key]
    assert_equal "run-123", workflow.reload.last_run_id
    assert_equal Time.utc(2026, 8, 19, 12, 5), workflow.last_run_at
  end

  private

  def create_workflow(principal: nil)
    AuthoredWorkflow.create!(
      name: "Incident summary #{SecureRandom.hex(4)}",
      prompt: "Summarize open incidents.",
      author: users(:acme_admin),
      principal: principal,
      delivery_channel: "C0123456789",
      cron_expression: "0 * * * *",
      enabled: true
    )
  end
end
