# Read model behind the console workflow pages (GET /api/v1/workflows...).
#
# Runs come from the api-rs read-only workflow views (CentaurWorkflowRun).
# Schedules and run details (input/result/failure) are best-effort enrichment
# from the workflows API; the payloads omit them when the API is unreachable.
# Raises Unavailable when the workflow views are missing or unreadable.
class WorkflowDashboard
  class Unavailable < StandardError
    def initialize(message = "Workflow database is unavailable. Console needs the API workflow read-only views to show runs.")
      super
    end
  end

  PER_PAGE = 50
  STATUS_ORDER = %w[running pending sleeping completed failed cancelled].freeze

  ENGINE_LABELS = {
    "codex" => "Codex",
    "claudecode" => "Claude Code",
    "amp" => "Amp",
    "nanocodex" => "Nanocodex",
    "pi" => "Pi",
    "omp" => "OMP"
  }.freeze

  def initialize(api_client:, logger: Rails.logger)
    @api_client = api_client
    @logger = logger
  end

  # One row per workflow (its latest run) with the per-queue breakdown.
  def index(page:)
    with_workflow_database do
      total = CentaurWorkflowRun.workflow_count
      meta = page_meta(total, page)
      runs = CentaurWorkflowRun.latest_per_workflow(limit: PER_PAGE, offset: (meta[:page] - 1) * PER_PAGE)
      breakdown = CentaurWorkflowRun.latest_per_queue(runs.map(&:workflow_key))
      [ runs.map { |run| workflow_row(run, breakdown[run.workflow_key]) }, meta ]
    end
  end

  # Overview, status counts, schedule, and debugging details for one workflow,
  # or nil when it has no runs.
  def show(workflow_name)
    with_workflow_database do
      latest_run = CentaurWorkflowRun.for_workflow(workflow_name, limit: 1).first
      next nil if latest_run.blank?

      status_counts = CentaurWorkflowRun.status_counts(workflow_name)
      queue_names = CentaurWorkflowRun.queue_names(workflow_name)
      schedule = schedules_for(workflow_name).first
      overview(workflow_name, latest_run, status_counts, queue_names, schedule).merge(
        debug: debug_rows(workflow_name, latest_run, status_counts, schedule)
      )
    end
  end

  # One page of a workflow's runs, optionally filtered by status and queue.
  def runs(workflow_name, status:, queue:, page:)
    with_workflow_database do
      total = CentaurWorkflowRun.run_count(workflow_name, status: status, queue: queue)
      meta = page_meta(total, page)
      runs = CentaurWorkflowRun.for_workflow(
        workflow_name, limit: PER_PAGE, offset: (meta[:page] - 1) * PER_PAGE, status: status, queue: queue
      )
      [ runs.map { |run| run_row(run) }, meta ]
    end
  end

  # Enqueues a run through the workflows API. Scheduled workflows start with
  # their registered schedule input so a forced run matches a normal tick.
  # Returns { run_id:, created: }; raises on API errors.
  def force_start(workflow_name)
    schedule = schedules_for(workflow_name).first
    result = api_client.create_workflow_run(workflow_name: workflow_name, input: schedule&.dig("input"))
    { run_id: result["run_id"], created: result["created"] != false }
  end

  def self.engine_label(harness_type)
    value = harness_type.to_s
    return nil if value.empty?

    ENGINE_LABELS.fetch(value) { value.tr("_-", " ").squish.split.map(&:capitalize).join(" ") }
  end

  # "cron */5 * * * *" or "every 5m" from the serde-tagged schedule kind
  # {"type":"cron","cron":...} | {"type":"interval","interval_seconds":...}.
  def self.schedule_label(schedule)
    kind = schedule.is_a?(Hash) ? schedule["kind"] : nil
    return nil unless kind.is_a?(Hash)

    case kind["type"]
    when "cron"
      "cron #{kind["cron"]}"
    when "interval"
      seconds = kind["interval_seconds"].to_i
      "every #{seconds % 60 == 0 && seconds >= 60 ? "#{seconds / 60}m" : "#{seconds}s"}"
    end
  end

  # GitHub URL for a repo-relative workflow source path. An overlay-repo
  # prefix maps to the overlay repository, everything else to the main repo.
  def self.source_url(source_path)
    path = source_path.to_s
    return nil if path.blank?

    if path.start_with?("centaur-tempo/")
      "https://github.com/tempoxyz/centaur-tempo/blob/main/#{path.delete_prefix("centaur-tempo/")}"
    else
      "https://github.com/paradigmxyz/centaur/blob/main/#{path}"
    end
  end

  def self.duration_label(run)
    started_at = run.started_or_created_at
    finished_at = run.terminal_at
    return "running" if started_at.present? && finished_at.blank? && run.display_status == "running"
    return "—" if started_at.blank? || finished_at.blank?

    ActionController::Base.helpers.distance_of_time_in_words(started_at, finished_at)
  end

  private

  attr_reader :api_client, :logger

  def with_workflow_database
    raise Unavailable unless CentaurWorkflowRun.available?

    yield
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    logger.warn("console_workflows_load_failed error=#{e.class}: #{e.message}")
    raise Unavailable
  end

  def page_meta(total, page)
    total_pages = [ (total.to_f / PER_PAGE).ceil, 1 ].max
    { page: page.clamp(1, total_pages), limit: PER_PAGE, total: total, total_pages: total_pages }
  end

  def run_timing(run)
    {
      status: run.display_status,
      queue_name: run.queue_name,
      queue_label: run.queue_label,
      attempts: run.attempts || 0,
      max_attempts: run.max_attempts.presence,
      started_at: run.started_or_created_at,
      created_at: run.created_at,
      finished_at: run.terminal_at
    }
  end

  # Index rows deliberately omit run and task ids.
  def workflow_row(run, queue_runs)
    {
      workflow_key: run.workflow_key,
      label: run.workflow_name_label,
      task_name: run.task_name.presence
    }.merge(run_timing(run)).merge(
      queues: Array(queue_runs).map do |queue_run|
        {
          queue_name: queue_run.queue_name,
          queue_label: queue_run.queue_label,
          status: queue_run.display_status,
          last_activity_at: queue_run.recency_at,
          run_count: queue_run.queue_run_count.to_i
        }
      end
    )
  end

  def run_row(run)
    { run_id: run.run_id, task_id: run.task_id }.merge(run_timing(run))
  end

  def overview(workflow_name, latest_run, status_counts, queue_names, schedule)
    {
      workflow_key: workflow_name,
      label: latest_run.workflow_name_label,
      task_name: latest_run.task_name.presence
    }.merge(run_timing(latest_run)).merge(
      harness_type: latest_run.harness_type.presence,
      engine_label: self.class.engine_label(latest_run.harness_type),
      duration_label: self.class.duration_label(latest_run),
      run_count: status_counts.values.sum,
      status_counts: ordered_status_counts(status_counts),
      queues: queue_names.map { |name| { queue_name: name, queue_label: CentaurWorkflowRun.queue_label_for(name) } },
      schedule: schedule_payload(schedule)
    )
  end

  def ordered_status_counts(status_counts)
    keys = (STATUS_ORDER & status_counts.keys) + (status_counts.keys - STATUS_ORDER).sort
    keys.map { |status| { status: status, count: status_counts[status] } }
  end

  # The registered schedule without its input (the debug rows show run input).
  def schedule_payload(schedule)
    return nil unless schedule

    kind = schedule["kind"].is_a?(Hash) ? schedule["kind"] : {}
    label = self.class.schedule_label(schedule)
    summary = [ label, (schedule["timezone"].presence if kind["type"] == "cron"),
                ("disabled" if schedule["enabled"] == false) ].compact.join(" · ")
    source_path = schedule["source_path"].presence
    {
      label: label,
      summary: summary.presence,
      timezone: schedule["timezone"].presence,
      enabled: schedule["enabled"] != false,
      source_path: source_path,
      source_url: self.class.source_url(source_path)
    }
  end

  def debug_rows(workflow_name, latest_run, status_counts, schedule)
    rows = []
    detail = fetch_run_detail(latest_run.run_id)
    if detail.present?
      input = detail["input"]
      rows << debug_row("Input", input) unless input.nil? || input == {} || input == ""
      rows << debug_row("Result", detail["result"]) unless detail["result"].nil?
      rows << debug_row("Failure", detail["failure"], detail["run_id"]) unless detail["failure"].nil?
    end

    failure = latest_failure_detail(workflow_name, latest_run, status_counts, detail, schedule)
    rows << debug_row("Last failure", failure["failure"], failure["run_id"]) if failure.present? && failure["failure"].present?
    rows
  end

  def latest_failure_detail(workflow_name, latest_run, status_counts, detail, schedule)
    return if detail.blank? && schedule.blank?
    return if latest_run.display_status == "failed"
    return unless status_counts["failed"].to_i.positive?

    failed_run = CentaurWorkflowRun.for_workflow(workflow_name, limit: 1, status: "failed").first
    fetch_run_detail(failed_run&.run_id)
  end

  def debug_row(label, value, run_id = nil)
    { label: label, value: value, run_id: run_id }
  end

  def schedules_for(workflow_name)
    response = api_client.list_workflow_schedules
    Array(response["schedules"]).select do |schedule|
      schedule.is_a?(Hash) && schedule["workflow_name"] == workflow_name
    end
  rescue StandardError => e
    logger.warn("console_workflow_schedules_failed error=#{e.class}: #{e.message}")
    []
  end

  def fetch_run_detail(run_id)
    return nil if run_id.blank?

    detail = api_client.get_workflow_run(run_id)["run"]
    detail.is_a?(Hash) ? detail : nil
  rescue StandardError => e
    logger.warn("console_workflow_run_detail_failed run=#{run_id} error=#{e.class}: #{e.message}")
    nil
  end
end
