require "test_helper"

class CentaurWorkflowRunTest < ActiveSupport::TestCase
  setup do
    ensure_workflow_runs_table
    CentaurWorkflowRun.reset_column_information
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
    CentaurWorkflowRun.new({
      queue_name: "centaur_workflows",
      workflow_name: "echo",
      task_name: "centaur_workflow",
      state: "pending",
      claimed: false
    }.merge(attrs))
  end
end
