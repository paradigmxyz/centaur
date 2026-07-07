class CentaurWorkflowRun < CentaurSessionRecord
  self.table_name = "centaur_readonly_workflow_runs"
  self.primary_key = "run_id"

  RECENCY_SQL =
    "coalesce(completed_at, failed_at, cancelled_at, started_at, " \
      "first_started_at, available_at, created_at)".freeze

  RECENT_ORDER = Arel.sql("#{RECENCY_SQL} desc, task_id desc")

  # Mirrors #workflow_key: runs are grouped under their workflow name, falling
  # back to the task name for runs enqueued before workflow names were recorded.
  WORKFLOW_KEY_SQL =
    "coalesce(nullif(workflow_name, ''), nullif(task_name, ''))".freeze

  # SQL twin of #display_status so status filters and tab counts agree with the
  # badge rendered for each row.
  DISPLAY_STATUS_SQL = <<~SQL.squish.freeze
    CASE
      WHEN cancelled_at IS NOT NULL THEN 'cancelled'
      WHEN failed_at IS NOT NULL THEN 'failed'
      WHEN completed_at IS NOT NULL THEN 'completed'
      WHEN claimed OR state = 'running' THEN 'running'
      ELSE coalesce(nullif(state, ''), 'unknown')
    END
  SQL

  scope :recent_first, -> { order(RECENT_ORDER) }

  class << self
    def available?
      connection.data_source_exists?(table_name)
    end

    def recent(limit:)
      recent_first.limit(limit).to_a
    end

    # The most recent run of each distinct workflow, newest activity first.
    def latest_per_workflow(limit:, offset: 0)
      find_by_sql([ <<~SQL, { limit: limit, offset: offset } ])
        SELECT * FROM (
          SELECT DISTINCT ON (#{WORKFLOW_KEY_SQL}) *
          FROM #{table_name}
          ORDER BY #{WORKFLOW_KEY_SQL}, #{RECENCY_SQL} DESC, task_id DESC
        ) latest_runs
        ORDER BY #{RECENCY_SQL} DESC, task_id DESC
        LIMIT :limit OFFSET :offset
      SQL
    end

    def workflow_count
      count_by_sql(
        "SELECT COUNT(*) FROM " \
          "(SELECT DISTINCT #{WORKFLOW_KEY_SQL} FROM #{table_name}) workflow_keys"
      )
    end

    # The most recent run per (workflow, queue) for the given workflow keys,
    # grouped by workflow key. Each run carries a queue_run_count attribute with
    # that queue's total run count. Feeds the per-queue lines on the index.
    def latest_per_queue(workflow_keys)
      keys = workflow_keys.compact.uniq
      return {} if keys.empty?

      runs = find_by_sql([ <<~SQL, { keys: keys } ])
        SELECT DISTINCT ON (#{WORKFLOW_KEY_SQL}, queue_name) *,
          COUNT(*) OVER (PARTITION BY #{WORKFLOW_KEY_SQL}, queue_name) AS queue_run_count
        FROM #{table_name}
        WHERE #{WORKFLOW_KEY_SQL} IN (:keys)
        ORDER BY #{WORKFLOW_KEY_SQL}, queue_name, #{RECENCY_SQL} DESC, task_id DESC
      SQL

      runs
        .group_by(&:workflow_key)
        .transform_values { |queue_runs| queue_runs.sort_by { |run| run.recency_at&.to_time.to_i }.reverse }
    end

    def for_workflow(workflow_name, limit:, offset: 0, status: nil, queue: nil)
      workflow_scope(workflow_name, status: status, queue: queue)
        .recent_first
        .limit(limit)
        .offset(offset)
        .to_a
    end

    def run_count(workflow_name, status: nil, queue: nil)
      workflow_scope(workflow_name, status: status, queue: queue).count
    end

    # { "completed" => 12, "running" => 1, ... } for one workflow's runs.
    def status_counts(workflow_name)
      workflow_scope(workflow_name)
        .group(Arel.sql(DISPLAY_STATUS_SQL))
        .count
    end

    def queue_names(workflow_name)
      workflow_scope(workflow_name)
        .distinct
        .order(:queue_name)
        .pluck(:queue_name)
    end

    def queue_label_for(queue_name)
      suffix = queue_name.to_s.delete_prefix("centaur_workflows").delete_prefix("_")
      suffix.presence&.tr("_", " ") || "default"
    end

    private

    def workflow_scope(workflow_name, status: nil, queue: nil)
      scope = where(
        "workflow_name = :workflow_name OR " \
          "((workflow_name IS NULL OR workflow_name = '') AND task_name = :workflow_name)",
        workflow_name: workflow_name
      )
      scope = scope.where("#{DISPLAY_STATUS_SQL} = ?", status) if status.present?
      scope = scope.where(queue_name: queue) if queue.present?
      scope
    end
  end

  def readonly? = true

  def workflow_name_label
    workflow_name.presence || task_name.presence || "unknown workflow"
  end

  def workflow_key
    workflow_name.presence || task_name.presence
  end

  def queue_label
    self.class.queue_label_for(queue_name)
  end

  def display_status
    return "cancelled" if cancelled_at.present?
    return "failed" if failed_at.present?
    return "completed" if completed_at.present?
    return "running" if claimed || state == "running"

    state.presence || "unknown"
  end

  def started_or_created_at
    started_at || first_started_at || created_at
  end

  def terminal_at
    completed_at || failed_at || cancelled_at
  end

  def recency_at
    terminal_at || started_at || first_started_at || available_at || created_at
  end
end
