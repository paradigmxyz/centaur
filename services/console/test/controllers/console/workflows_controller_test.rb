require "test_helper"

class Console::WorkflowsControllerTest < ActionDispatch::IntegrationTest
  FakeWorkflowRun = Struct.new(
    :workflow_name,
    :workflow_name_label,
    :task_name,
    :display_status,
    :queue_label,
    :attempts,
    :max_attempts,
    :started_or_created_at,
    :created_at,
    :terminal_at,
    :run_id,
    :task_id,
    :harness_type,
    keyword_init: true
  ) do
    def workflow_name_label
      self[:workflow_name_label].presence || workflow_name.presence || task_name.presence || "unknown workflow"
    end

    def workflow_key
      workflow_name.presence || task_name.presence
    end
  end

  setup do
    @operator = users(:acme_admin)
    post login_url, params: { email: @operator.email, password: "password123456" }
  end

  test "an admin sees workflow runs" do
    run = fake_run(workflow_name: "slack_sync", display_status: "running")

    with_workflow_runs(run) do
      get console_workflows_url
    end

    assert_response :ok
    assert_select "h1", count: 0
    assert_select ".console-thread-group-title-active", text: /Workflows/
    assert_select "a[href=?]", console_workflow_path("slack_sync"), text: /slack_sync/
    assert_select "span", text: "running"
    assert_select "a[href=?]", console_workflows_path
    assert response.body.index('href="/console/workflows"') < response.body.index('href="/console/threads"')
  end

  test "a non-admin is redirected away from the workflow dashboard" do
    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    get console_workflows_url

    assert_redirected_to console_threads_path
    assert_nil flash[:alert]
  end

  test "a non-admin does not see the workflows tab" do
    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    get console_threads_url

    assert_response :ok
    assert_select ".console-nav-link", text: "Control", count: 0
    assert_select ".console-nav-link", text: "Data Sync", count: 0
    assert_select ".console-thread-group-title", text: /Chats/
    assert_select ".console-thread-group-title", text: /Workflows/, count: 0
  end

  test "workflow show page lists core metadata and historical runs" do
    run = fake_run(workflow_name: "slack_sync", display_status: "completed", harness_type: "codex")

    with_workflow_history("slack_sync", run) do
      get console_workflow_url("slack_sync")
    end

    assert_response :ok
    assert_select "dt", text: "Workflow"
    assert_select "dd", text: /slack_sync/
    assert_select "dt", text: "Engine"
    assert_select "dd", text: "codex"
    assert_select "h1", "Historical Runs"
    assert_select "tbody tr", count: 1
  end

  test "workflow show page returns not found for unknown workflow" do
    with_workflow_history("missing") do
      get console_workflow_url("missing")
    end

    assert_response :not_found
    assert_select "body", text: /No workflow runs found for missing/
  end

  test "workflows page handles unavailable workflow database" do
    with_centaur_workflow_run_methods(available?: -> { false }) do
      get console_workflows_url
    end

    assert_response :ok
    assert_select "body", text: /Workflow database is unavailable/
    assert_select "body", text: /No workflow runs available/
  end

  private

  def fake_run(attrs = {})
    now = Time.zone.parse("2026-07-06 12:00:00 UTC")
    FakeWorkflowRun.new({
      workflow_name: "echo",
      workflow_name_label: nil,
      task_name: "centaur_workflow",
      display_status: "completed",
      queue_label: "default",
      attempts: 1,
      max_attempts: 3,
      started_or_created_at: now,
      created_at: now,
      terminal_at: now + 2.minutes,
      run_id: "00000000-0000-0000-0000-000000000001",
      task_id: "00000000-0000-0000-0000-000000000002",
      harness_type: nil
    }.merge(attrs))
  end

  def with_workflow_runs(*runs)
    with_centaur_workflow_run_methods(
      available?: -> { true },
      recent: ->(limit:) {
        runs
      }
    ) do
      yield
    end
  end

  def with_workflow_history(workflow_name, *runs)
    with_centaur_workflow_run_methods(
      available?: -> { true },
      for_workflow: ->(name, limit:) {
        name == workflow_name && limit.positive? ? runs : []
      }
    ) do
      yield
    end
  end

  def with_centaur_workflow_run_methods(overrides)
    originals = overrides.keys.to_h { |name| [ name, CentaurWorkflowRun.method(name) ] }

    overrides.each do |name, implementation|
      CentaurWorkflowRun.define_singleton_method(name, &implementation)
    end

    yield
  ensure
    originals&.each do |name, original|
      CentaurWorkflowRun.define_singleton_method(name, original)
    end
  end
end
