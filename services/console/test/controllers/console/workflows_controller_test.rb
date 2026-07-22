require "test_helper"

class Console::WorkflowsControllerTest < ActionDispatch::IntegrationTest
  FakeWorkflowRun = Struct.new(
    :workflow_name,
    :workflow_name_label,
    :task_name,
    :display_status,
    :queue_name,
    :queue_label,
    :attempts,
    :max_attempts,
    :started_or_created_at,
    :created_at,
    :terminal_at,
    :recency_at,
    :run_id,
    :task_id,
    :harness_type,
    :queue_run_count,
    keyword_init: true
  ) do
    def workflow_name_label
      self[:workflow_name_label].presence || workflow_name.presence || task_name.presence || "unknown workflow"
    end

    def workflow_key
      workflow_name.presence || task_name.presence
    end

    def recency_at
      self[:recency_at] || terminal_at || started_or_created_at
    end
  end

  # Stands in for CentaurApiClient: schedules/run details for show-page
  # enrichment, plus a capture of force-started runs.
  class FakeApiClient
    attr_reader :created_runs

    def initialize(schedules: [], run_details: {}, create_result: nil, create_error: nil)
      @schedules = schedules
      @run_details = run_details
      @create_result = create_result || { "ok" => true, "run_id" => "run-new", "created" => true }
      @create_error = create_error
      @created_runs = []
    end

    def list_workflow_schedules
      { "ok" => true, "schedules" => @schedules }
    end

    def get_workflow_run(run_id)
      detail = @run_details[run_id]
      raise CentaurApiClient::Error, "run not found" unless detail

      { "ok" => true, "run" => detail }
    end

    def create_workflow_run(workflow_name:, input: nil)
      raise CentaurApiClient::Error, @create_error if @create_error

      @created_runs << { workflow_name: workflow_name, input: input }
      @create_result
    end
  end

  setup do
    @original_client_factory = Console::WorkflowsController.client_factory
    with_api_client(FakeApiClient.new)
    @operator = users(:acme_admin)
    post login_url, params: { email: @operator.email, password: "password123456" }
  end

  teardown do
    Console::WorkflowsController.client_factory = @original_client_factory
  end

  test "an admin sees one row per workflow" do
    run = fake_run(workflow_name: "slack_sync", display_status: "running")

    with_workflow_index(runs: [ run ]) do
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

  test "a workflow with runs in several queues lists each queue on its own line" do
    run = fake_run(workflow_name: "slack_backfill", queue_name: "centaur_workflows_etl_backfill", queue_label: "etl backfill")
    queue_runs = [
      fake_run(workflow_name: "slack_backfill", queue_name: "centaur_workflows_etl_backfill", queue_label: "etl backfill", queue_run_count: 7),
      fake_run(workflow_name: "slack_backfill", queue_name: "centaur_workflows_slack_live", queue_label: "slack live", display_status: "running", queue_run_count: 2)
    ]

    with_workflow_index(runs: [ run ], queue_breakdown: { "slack_backfill" => queue_runs }) do
      get console_workflows_url
    end

    assert_response :ok
    assert_select "tbody tr", count: 1
    assert_match "etl backfill", response.body
    assert_match "slack live", response.body
    assert_match "├", response.body
    assert_match "└", response.body
    assert_match "7 runs", response.body
  end

  test "the workflow index does not show run ids" do
    run = fake_run(workflow_name: "slack_sync")

    with_workflow_index(runs: [ run ]) do
      get console_workflows_url
    end

    assert_response :ok
    assert_no_match run.run_id, response.body
    assert_no_match run.task_id, response.body
  end

  test "the workflow index is paginated" do
    runs = 3.times.map { |i| fake_run(workflow_name: "wf_#{i}") }

    with_workflow_index(runs: runs, workflow_count: 120) do
      get console_workflows_url, params: { page: 2 }
    end

    assert_response :ok
    assert_match "120 workflows", response.body
    assert_match "page 2 of 3", response.body
    assert_select "a", text: "Previous"
    assert_select "a", text: "Next"
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

    with_workflow_history("slack_sync", runs: [ run ]) do
      get console_workflow_url("slack_sync")
    end

    assert_response :ok
    assert_select "h1.page-title", text: /slack_sync/
    assert_select "dt", text: "Engine"
    assert_select "dd", text: "Codex"
    assert_select "h2", "Historical Runs"
    assert_select "tbody tr", count: 1
    assert_select "form[action=?]", run_console_workflow_path("slack_sync")
  end

  test "workflow show page renders status filter tabs with counts" do
    run = fake_run(workflow_name: "slack_sync", display_status: "completed")

    with_workflow_history(
      "slack_sync",
      runs: [ run ],
      status_counts: { "completed" => 9, "failed" => 1 }
    ) do
      get console_workflow_url("slack_sync")
    end

    assert_response :ok
    assert_select "a.chip", text: /all\s*10/
    assert_select "a.chip", text: /completed\s*9/
    assert_select "a.chip", text: /failed\s*1/
    assert_select "dd", text: /10 runs/
  end

  test "workflow show page marks the active status tab and passes the filter through" do
    run = fake_run(workflow_name: "slack_sync", display_status: "failed")
    seen = {}

    with_workflow_history(
      "slack_sync",
      runs: [ run ],
      status_counts: { "completed" => 9, "failed" => 1 },
      capture: seen
    ) do
      get console_workflow_url("slack_sync"), params: { status: "failed" }
    end

    assert_response :ok
    assert_equal "failed", seen[:status]
    assert_select "a.chip-on", text: /failed\s*1/
  end

  test "workflow show page renders queue tabs when several queues exist" do
    run = fake_run(workflow_name: "slack_sync")

    with_workflow_history(
      "slack_sync",
      runs: [ run ],
      queue_names: %w[centaur_workflows_etl centaur_workflows_slack_live]
    ) do
      get console_workflow_url("slack_sync"), params: { queue: "centaur_workflows_slack_live" }
    end

    assert_response :ok
    assert_select "a.chip", text: "etl"
    assert_select "a.chip-on", text: "slack live"
  end

  test "workflow show page paginates historical runs" do
    runs = 2.times.map { |i| fake_run(workflow_name: "slack_sync", run_id: "run-#{i}") }

    with_workflow_history("slack_sync", runs: runs, run_count: 130) do
      get console_workflow_url("slack_sync"), params: { page: 2 }
    end

    assert_response :ok
    assert_match "130 runs", response.body
    assert_match "page 2 of 3", response.body
  end

  test "workflow show page shows the schedule and source link when registered" do
    run = fake_run(workflow_name: "slack_sync", harness_type: "codex")
    with_api_client(FakeApiClient.new(schedules: [ slack_sync_schedule ]))

    with_workflow_history("slack_sync", runs: [ run ]) do
      get console_workflow_url("slack_sync")
    end

    assert_response :ok
    assert_select "dt", text: "Schedule"
    assert_select "dd", text: /cron \*\/5 \* \* \* \* · America\/Los_Angeles/
    assert_select "a[href=?]",
                  "https://github.com/paradigmxyz/centaur/blob/main/workflows/slack/sync.py",
                  text: /workflows\/slack\/sync\.py/
  end

  test "workflow show page links overlay-repo workflow sources to the overlay repo" do
    run = fake_run(workflow_name: "consensus_ci_triage")
    schedule = slack_sync_schedule.merge(
      "workflow_name" => "consensus_ci_triage",
      "source_path" => "centaur-tempo/workflows/consensus_ci_triage.py"
    )
    with_api_client(FakeApiClient.new(schedules: [ schedule ]))

    with_workflow_history("consensus_ci_triage", runs: [ run ]) do
      get console_workflow_url("consensus_ci_triage")
    end

    assert_response :ok
    assert_select "a[href=?]",
                  "https://github.com/tempoxyz/centaur-tempo/blob/main/workflows/consensus_ci_triage.py"
  end

  test "workflow show page surfaces the latest run's input and failure for debugging" do
    run = fake_run(workflow_name: "slack_sync", display_status: "failed")
    with_api_client(
      FakeApiClient.new(
        run_details: {
          run.run_id => {
            "run_id" => run.run_id,
            "input" => { "mode" => "full" },
            "failure" => { "error" => "boom exploded" }
          }
        }
      )
    )

    with_workflow_history("slack_sync", runs: [ run ]) do
      get console_workflow_url("slack_sync")
    end

    assert_response :ok
    assert_select "h2", text: "Debugging"
    assert_select "dt", text: "Input"
    assert_select "dt", text: "Failure"
    assert_match "boom exploded", response.body
  end

  test "workflow show page renders without api enrichment when the api is down" do
    run = fake_run(workflow_name: "slack_sync")
    with_api_client(FakeApiClient.new(run_details: {}))

    with_workflow_history("slack_sync", runs: [ run ]) do
      get console_workflow_url("slack_sync")
    end

    assert_response :ok
    assert_select "h2", text: "Debugging", count: 0
    assert_select "dt", text: "Schedule", count: 0
  end

  test "force starting a workflow queues a run with the schedule input" do
    client = FakeApiClient.new(schedules: [ slack_sync_schedule ])
    with_api_client(client)

    post run_console_workflow_url("slack_sync")

    assert_redirected_to console_workflow_path("slack_sync")
    assert_match(/Run queued \(run-new\)/, flash[:notice])
    assert_equal [ { workflow_name: "slack_sync", input: { "mode" => "incremental" } } ], client.created_runs
  end

  test "force starting a workflow surfaces api errors" do
    with_api_client(FakeApiClient.new(create_error: "workflow runtime is not enabled"))

    post run_console_workflow_url("slack_sync")

    assert_redirected_to console_workflow_path("slack_sync")
    assert_match(/workflow runtime is not enabled/, flash[:alert])
  end

  test "a non-admin cannot force start a workflow" do
    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }
    client = FakeApiClient.new
    with_api_client(client)

    post run_console_workflow_url("slack_sync")

    assert_redirected_to console_threads_path
    assert_empty client.created_runs
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

  def with_api_client(client)
    Console::WorkflowsController.client_factory = -> { client }
  end

  def slack_sync_schedule
    {
      "schedule_id" => "slack_sync",
      "workflow_name" => "slack_sync",
      "source_path" => "workflows/slack/sync.py",
      "kind" => { "type" => "cron", "cron" => "*/5 * * * *" },
      "timezone" => "America/Los_Angeles",
      "input" => { "mode" => "incremental" },
      "enabled" => true,
      "no_delivery" => false
    }
  end

  def fake_run(attrs = {})
    now = Time.zone.parse("2026-07-06 12:00:00 UTC")
    FakeWorkflowRun.new({
      workflow_name: "echo",
      workflow_name_label: nil,
      task_name: "centaur_workflow",
      display_status: "completed",
      queue_name: "centaur_workflows",
      queue_label: "default",
      attempts: 1,
      max_attempts: 3,
      started_or_created_at: now,
      created_at: now,
      terminal_at: now + 2.minutes,
      recency_at: nil,
      run_id: "00000000-0000-0000-0000-000000000001",
      task_id: "00000000-0000-0000-0000-000000000002",
      harness_type: nil,
      queue_run_count: 1
    }.merge(attrs))
  end

  def with_workflow_index(runs:, queue_breakdown: {}, workflow_count: nil)
    with_centaur_workflow_run_methods(
      available?: -> { true },
      workflow_count: -> { workflow_count || runs.size },
      latest_per_workflow: ->(limit:, offset: 0) { runs },
      latest_per_queue: ->(keys) { queue_breakdown }
    ) do
      yield
    end
  end

  def with_workflow_history(workflow_name, runs: [], status_counts: nil, queue_names: [], run_count: nil, capture: nil)
    status_counts ||= runs.group_by(&:display_status).transform_values(&:size)
    with_centaur_workflow_run_methods(
      available?: -> { true },
      for_workflow: ->(name, limit:, offset: 0, status: nil, queue: nil) {
        capture&.merge!(status: status, queue: queue, offset: offset)
        name == workflow_name && limit.positive? ? runs : []
      },
      status_counts: ->(name) { name == workflow_name ? status_counts : {} },
      queue_names: ->(name) { name == workflow_name ? queue_names : [] },
      run_count: ->(name, status: nil, queue: nil) { run_count || runs.size }
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
