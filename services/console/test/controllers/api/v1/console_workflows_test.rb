require "test_helper"

module Api
  module V1
    # Requests use the console session cookie (+ CSRF) as the SPA does; bearer
    # access is covered by the authorization test.
    class ConsoleWorkflowsTest < ActionDispatch::IntegrationTest
      FakeWorkflowRun = Struct.new(
        :workflow_name, :workflow_name_label, :task_name, :display_status, :queue_name, :queue_label,
        :attempts, :max_attempts, :started_or_created_at, :created_at, :terminal_at, :recency_at,
        :run_id, :task_id, :harness_type, :queue_run_count,
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

      # Stands in for CentaurApiClient: schedules/run details for show
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
        @original_client_factory = Api::V1::WorkflowsController.client_factory
        with_api_client(FakeApiClient.new)
        sign_in_console(users(:acme_admin))
      end

      teardown do
        Api::V1::WorkflowsController.client_factory = @original_client_factory
      end

      test "workflow endpoints are admin-only for every caller type" do
        client = FakeApiClient.new
        with_api_client(client)
        with_workflow_history("slack_sync", runs: [ fake_run(workflow_name: "slack_sync") ]) do
          assert_admin_only_api :get, api_v1_workflows_path
          assert_admin_only_api :get, api_v1_workflow_path("slack_sync")
          assert_admin_only_api :get, runs_api_v1_workflow_path("slack_sync")
          assert_admin_only_api :post, runs_api_v1_workflow_path("slack_sync"), params: {}
        end
        assert_empty client.created_runs
      end

      test "an admin sees one row per workflow" do
        run = fake_run(workflow_name: "slack_sync", display_status: "running", harness_type: "codex")

        with_workflow_index(runs: [ run ]) do
          get api_v1_workflows_path
        end

        assert_response :ok
        assert_equal "no-store", response.headers["Cache-Control"]
        row = json_body["data"].sole
        assert_equal "slack_sync", row["workflow_key"]
        assert_equal "slack_sync", row["label"]
        assert_equal "centaur_workflow", row["task_name"]
        assert_equal "running", row["status"]
        assert_equal "default", row["queue_label"]
        assert_equal 1, row["attempts"]
        assert_equal 3, row["max_attempts"]
        assert_equal({ "page" => 1, "limit" => 50, "total" => 1, "total_pages" => 1 }, json_body["meta"])
      end

      test "the index lists the per-queue breakdown" do
        run = fake_run(workflow_name: "slack_sync")
        queues = [
          fake_run(workflow_name: "slack_sync", queue_name: "centaur_workflows_backfill", queue_label: "backfill",
                   display_status: "failed", queue_run_count: 4),
          fake_run(workflow_name: "slack_sync", queue_run_count: 9)
        ]

        with_workflow_index(runs: [ run ], queue_breakdown: { "slack_sync" => queues }) do
          get api_v1_workflows_path
        end

        assert_equal [ [ "backfill", "failed", 4 ], [ "default", "completed", 9 ] ],
                     json_body["data"].sole["queues"].map { |q| q.values_at("queue_label", "status", "run_count") }
      end

      test "the workflow index does not show run ids" do
        run = fake_run(workflow_name: "slack_sync")

        with_workflow_index(runs: [ run ]) do
          get api_v1_workflows_path
        end

        assert_response :ok
        assert_no_match run.run_id, response.body
        assert_no_match run.task_id, response.body
      end

      test "the index paginates by workflow count and clamps the page" do
        seen = {}
        with_workflow_index(runs: [], workflow_count: 120, capture: seen) do
          get api_v1_workflows_path(page: 9)
        end

        assert_equal({ "page" => 3, "limit" => 50, "total" => 120, "total_pages" => 3 }, json_body["meta"])
        assert_equal 100, seen[:offset]
      end

      test "an unavailable workflow database is a 503 with a reason" do
        with_centaur_workflow_run_methods(available?: -> { false }) do
          get api_v1_workflows_path
          assert_response :service_unavailable
          assert_equal "workflow_database_unavailable", json_body.dig("error", "details", "reason")
          assert_match(/Workflow database is unavailable/, json_body.dig("error", "message"))

          get api_v1_workflow_path("slack_sync")
          assert_response :service_unavailable
        end

        with_centaur_workflow_run_methods(available?: -> { true },
                                          workflow_count: -> { raise ActiveRecord::StatementInvalid, "boom" }) do
          get api_v1_workflows_path
        end
        assert_response :service_unavailable
      end

      test "workflow show returns the overview with status counts in tab order" do
        run = fake_run(workflow_name: "slack_sync", display_status: "failed", harness_type: "claudecode")

        with_workflow_history(
          "slack_sync",
          runs: [ run ],
          status_counts: { "completed" => 9, "failed" => 1, "weird" => 2, "running" => 1 },
          queue_names: %w[centaur_workflows centaur_workflows_backfill]
        ) do
          get api_v1_workflow_path("slack_sync")
        end

        assert_response :ok
        data = json_body["data"]
        assert_equal "slack_sync", data["workflow_key"]
        assert_equal "failed", data["status"]
        assert_equal "Claude Code", data["engine_label"]
        assert_equal "2 minutes", data["duration_label"]
        assert_equal 13, data["run_count"]
        assert_equal %w[running completed failed weird], data["status_counts"].pluck("status")
        assert_equal %w[default backfill], data["queues"].pluck("queue_label")
      end

      test "workflow runs passes the status and queue filters through" do
        run = fake_run(workflow_name: "slack_sync", display_status: "failed")
        seen = {}

        with_workflow_history("slack_sync", runs: [ run ], capture: seen) do
          get runs_api_v1_workflow_path("slack_sync"), params: { status: "failed", queue: "centaur_workflows" }
        end

        assert_response :ok
        assert_equal "failed", seen[:status]
        assert_equal "centaur_workflows", seen[:queue]
        row = json_body["data"].sole
        assert_equal run.run_id, row["run_id"]
        assert_equal run.task_id, row["task_id"]
        assert_equal 1, json_body.dig("meta", "total")
      end

      test "workflow show renders without api enrichment when the api is down" do
        run = fake_run(workflow_name: "slack_sync")
        with_api_client(FakeApiClient.new(run_details: {}))

        with_workflow_history("slack_sync", runs: [ run ]) do
          get api_v1_workflow_path("slack_sync")
        end

        assert_response :ok
        assert_equal [], json_body.dig("data", "debug")
        assert_nil json_body.dig("data", "schedule")
      end

      test "workflow show includes the schedule and debugging details" do
        latest = fake_run(workflow_name: "slack_sync", run_id: "run-latest")
        failed = fake_run(workflow_name: "slack_sync", run_id: "run-failed", display_status: "failed")
        with_api_client(FakeApiClient.new(
          schedules: [ slack_sync_schedule ],
          run_details: {
            "run-latest" => { "run_id" => "run-latest", "input" => { "mode" => "incremental" }, "result" => { "ok" => 1 } },
            "run-failed" => { "run_id" => "run-failed", "failure" => { "message" => "boom" } }
          }
        ))

        with_workflow_history("slack_sync", runs: [ latest ], status_counts: { "completed" => 3, "failed" => 1 },
                                            failed_runs: [ failed ]) do
          get api_v1_workflow_path("slack_sync")
        end

        data = json_body["data"]
        assert_equal "cron */5 * * * * · America/Los_Angeles", data.dig("schedule", "summary")
        assert_equal "workflows/slack/sync.py", data.dig("schedule", "source_path")
        assert_match %r{/blob/main/workflows/slack/sync\.py\z}, data.dig("schedule", "source_url")
        assert_not data["schedule"].key?("input")
        assert_equal [ "Input", "Result", "Last failure" ], data["debug"].pluck("label")
        assert_equal({ "mode" => "incremental" }, data["debug"].first["value"])
        assert_equal "run-failed", data["debug"].last["run_id"]
      end

      test "force starting a workflow queues a run with the schedule input" do
        client = FakeApiClient.new(schedules: [ slack_sync_schedule ])
        with_api_client(client)

        post runs_api_v1_workflow_path("slack_sync"), headers: csrf_headers, as: :json

        assert_response :created
        assert_equal({ "run_id" => "run-new", "created" => true }, json_body["data"])
        assert_equal [ { workflow_name: "slack_sync", input: { "mode" => "incremental" } } ], client.created_runs
      end

      test "force starting reports an already queued idempotent run" do
        with_api_client(FakeApiClient.new(create_result: { "run_id" => "run-old", "created" => false }))

        post runs_api_v1_workflow_path("slack_sync"), headers: csrf_headers, as: :json

        assert_response :ok
        assert_equal({ "run_id" => "run-old", "created" => false }, json_body["data"])
      end

      test "force starting a workflow surfaces api errors" do
        with_api_client(FakeApiClient.new(create_error: "workflow runtime is not enabled"))

        post runs_api_v1_workflow_path("slack_sync"), headers: csrf_headers, as: :json

        assert_response :bad_gateway
        assert_match(/workflow runtime is not enabled/, json_body.dig("error", "message"))
        assert_equal "workflow_api_error", json_body.dig("error", "details", "reason")
      end

      test "workflow show returns not found for unknown workflow" do
        with_workflow_history("missing") do
          get api_v1_workflow_path("missing")
        end

        assert_response :not_found
        assert_equal "No workflow runs found for missing.", json_body.dig("error", "message")
      end

      test "workflow keys may contain dots" do
        with_workflow_history("team.sync", runs: [ fake_run(workflow_name: "team.sync") ]) do
          get api_v1_workflow_path("team.sync")
        end

        assert_response :ok
        assert_equal "team.sync", json_body.dig("data", "workflow_key")
      end

      test "labels and urls for engines, schedules, and sources" do
        assert_equal "Codex", WorkflowDashboard.engine_label("codex")
        assert_equal "Some New Thing", WorkflowDashboard.engine_label("some_new-thing")
        assert_nil WorkflowDashboard.engine_label(nil)
        assert_equal "every 5m", WorkflowDashboard.schedule_label({ "kind" => { "type" => "interval", "interval_seconds" => 300 } })
        assert_equal "every 45s", WorkflowDashboard.schedule_label({ "kind" => { "type" => "interval", "interval_seconds" => 45 } })
        assert_nil WorkflowDashboard.schedule_label({})
        assert_nil WorkflowDashboard.source_url("")
      end

      private

      def with_api_client(client)
        Api::V1::WorkflowsController.client_factory = -> { client }
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

      def with_workflow_index(runs:, queue_breakdown: {}, workflow_count: nil, capture: nil, &)
        with_centaur_workflow_run_methods(
          available?: -> { true },
          workflow_count: -> { workflow_count || runs.size },
          latest_per_workflow: ->(limit:, offset: 0) {
            capture&.merge!(offset: offset)
            runs
          },
          latest_per_queue: ->(_keys) { queue_breakdown },
          &
        )
      end

      def with_workflow_history(workflow_name, runs: [], status_counts: nil, queue_names: [], failed_runs: [],
                                capture: nil, &)
        status_counts ||= runs.group_by(&:display_status).transform_values(&:size)
        with_centaur_workflow_run_methods(
          available?: -> { true },
          for_workflow: ->(name, limit:, offset: 0, status: nil, queue: nil) {
            capture&.merge!(status: status, queue: queue, offset: offset)
            next [] unless name == workflow_name && limit.positive?

            status == "failed" && failed_runs.any? ? failed_runs : runs
          },
          status_counts: ->(name) { name == workflow_name ? status_counts : {} },
          queue_names: ->(name) { name == workflow_name ? queue_names : [] },
          run_count: ->(_name, status: nil, queue: nil) { runs.size },
          &
        )
      end

      def with_centaur_workflow_run_methods(overrides)
        originals = overrides.keys.to_h { |name| [ name, CentaurWorkflowRun.method(name) ] }
        overrides.each { |name, implementation| CentaurWorkflowRun.define_singleton_method(name, &implementation) }
        yield
      ensure
        originals&.each { |name, original| CentaurWorkflowRun.define_singleton_method(name, original) }
      end
    end
  end
end
