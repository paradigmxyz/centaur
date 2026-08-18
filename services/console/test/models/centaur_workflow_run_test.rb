require "test_helper"

class CentaurWorkflowRunTest < ActiveSupport::TestCase
  setup do
    ensure_workflow_runs_table
    CentaurWorkflowRun.reset_column_information
    CentaurWorkflowRun.delete_all
  end

  test "workflow runs are read only" do
    assert CentaurWorkflowRun.new.readonly?
  end

  test "display status derives useful terminal and running states" do
    assert_equal "cancelled", workflow_run(cancelled_at: Time.current).display_status
    assert_equal "failed", workflow_run(failed_at: Time.current).display_status
    assert_equal "completed", workflow_run(completed_at: Time.current).display_status
    assert_equal "running", workflow_run(claimed: true, state: "pending").display_status
    assert_equal "sleeping", workflow_run(state: "sleeping").display_status
  end

  test "queue and workflow labels have readable fallbacks" do
    run = workflow_run(queue_name: "centaur_workflows_etl", workflow_name: nil, task_name: "task")

    assert_equal "etl", run.queue_label
    assert_equal "task", run.workflow_name_label
  end

  test "queue label removes the common queue prefix" do
    run = workflow_run(queue_name: "centaur_workflows_etl_backfill")

    assert_equal "etl backfill", run.queue_label
  end

  test "latest_per_workflow returns one row per workflow, newest activity first" do
    insert_run(workflow_name: "alpha", completed_at: 3.hours.ago)
    insert_run(workflow_name: "alpha", completed_at: 1.hour.ago)
    insert_run(workflow_name: "beta", completed_at: 2.hours.ago)

    runs = CentaurWorkflowRun.latest_per_workflow(limit: 10)

    assert_equal %w[alpha beta], runs.map(&:workflow_key)
    assert_in_delta 1.hour.ago.to_i, runs.first.completed_at.to_i, 5
    assert_equal 2, CentaurWorkflowRun.workflow_count
  end

  test "latest_per_workflow groups blank workflow names under the task name" do
    insert_run(workflow_name: nil, task_name: "legacy_task", completed_at: 1.hour.ago)
    insert_run(workflow_name: "", task_name: "legacy_task", completed_at: 2.hours.ago)

    runs = CentaurWorkflowRun.latest_per_workflow(limit: 10)

    assert_equal [ "legacy_task" ], runs.map(&:workflow_key)
    assert_equal 1, CentaurWorkflowRun.workflow_count
  end

  test "latest_per_workflow paginates" do
    insert_run(workflow_name: "alpha", completed_at: 1.hour.ago)
    insert_run(workflow_name: "beta", completed_at: 2.hours.ago)
    insert_run(workflow_name: "gamma", completed_at: 3.hours.ago)

    page_two = CentaurWorkflowRun.latest_per_workflow(limit: 2, offset: 2)

    assert_equal %w[gamma], page_two.map(&:workflow_key)
  end

  test "latest_per_queue returns the newest run per queue with run counts" do
    insert_run(workflow_name: "alpha", queue_name: "centaur_workflows_etl", completed_at: 3.hours.ago)
    insert_run(workflow_name: "alpha", queue_name: "centaur_workflows_etl", completed_at: 2.hours.ago)
    insert_run(workflow_name: "alpha", queue_name: "centaur_workflows_live", completed_at: 1.hour.ago)
    insert_run(workflow_name: "beta", queue_name: "centaur_workflows_etl", completed_at: 1.hour.ago)

    breakdown = CentaurWorkflowRun.latest_per_queue(%w[alpha])

    assert_equal %w[alpha], breakdown.keys
    queue_runs = breakdown["alpha"]
    assert_equal %w[centaur_workflows_live centaur_workflows_etl], queue_runs.map(&:queue_name)
    assert_equal [ 1, 2 ], queue_runs.map { |run| run.queue_run_count.to_i }
  end

  test "for_workflow filters by status and queue and paginates" do
    insert_run(workflow_name: "alpha", queue_name: "centaur_workflows_etl", completed_at: 1.hour.ago)
    insert_run(workflow_name: "alpha", queue_name: "centaur_workflows_etl", failed_at: 2.hours.ago)
    insert_run(workflow_name: "alpha", queue_name: "centaur_workflows_live", completed_at: 3.hours.ago)

    completed = CentaurWorkflowRun.for_workflow("alpha", limit: 10, status: "completed")
    assert_equal 2, completed.size
    assert completed.all? { |run| run.display_status == "completed" }

    live_only = CentaurWorkflowRun.for_workflow("alpha", limit: 10, queue: "centaur_workflows_live")
    assert_equal 1, live_only.size

    page_two = CentaurWorkflowRun.for_workflow("alpha", limit: 2, offset: 2)
    assert_equal 1, page_two.size

    assert_equal 2, CentaurWorkflowRun.run_count("alpha", status: "completed")
    assert_equal 3, CentaurWorkflowRun.run_count("alpha")
  end

  test "status_counts and queue_names summarize a workflow's runs" do
    insert_run(workflow_name: "alpha", queue_name: "centaur_workflows_etl", completed_at: 1.hour.ago)
    insert_run(workflow_name: "alpha", queue_name: "centaur_workflows_live", failed_at: 2.hours.ago)
    insert_run(workflow_name: "alpha", queue_name: "centaur_workflows_live", claimed: true, state: "running")

    assert_equal(
      { "completed" => 1, "failed" => 1, "running" => 1 },
      CentaurWorkflowRun.status_counts("alpha")
    )
    assert_equal(
      %w[centaur_workflows_etl centaur_workflows_live],
      CentaurWorkflowRun.queue_names("alpha")
    )
  end

  private

  def ensure_workflow_runs_table
    return if CentaurWorkflowRun.connection.data_source_exists?(CentaurWorkflowRun.table_name)

    CentaurWorkflowRun.connection.create_table(
      CentaurWorkflowRun.table_name,
      id: false,
      temporary: true
    ) do |t|
      t.string :queue_name
      t.string :run_id
      t.string :task_id
      t.string :task_name
      t.string :workflow_name
      t.string :harness_type
      t.string :state
      t.integer :attempts
      t.integer :max_attempts
      t.datetime :created_at
      t.datetime :first_started_at
      t.datetime :started_at
      t.datetime :completed_at
      t.datetime :failed_at
      t.datetime :available_at
      t.boolean :claimed
      t.datetime :cancelled_at
    end
  end

  def workflow_run(attrs = {})
    CentaurWorkflowRun.new(default_run_attributes.merge(attrs))
  end

  def insert_run(attrs = {})
    @run_sequence = (@run_sequence || 0) + 1
    CentaurWorkflowRun.insert_all!(
      [
        default_run_attributes.merge(
          run_id: format("00000000-0000-0000-0000-%012d", @run_sequence),
          task_id: format("11111111-0000-0000-0000-%012d", @run_sequence),
          created_at: 1.day.ago
        ).merge(attrs)
      ],
      returning: false
    )
  end

  def default_run_attributes
    {
      queue_name: "centaur_workflows",
      workflow_name: "echo",
      task_name: "centaur_workflow",
      state: "pending",
      claimed: false
    }
  end
end
