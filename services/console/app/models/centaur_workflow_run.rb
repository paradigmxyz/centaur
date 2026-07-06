class CentaurWorkflowRun < CentaurSessionRecord
  self.table_name = "centaur_readonly_workflow_runs"
  self.primary_key = "run_id"

  RECENT_ORDER = Arel.sql(
    "coalesce(completed_at, failed_at, cancelled_at, started_at, " \
      "first_started_at, available_at, created_at) desc, task_id desc"
  )

  scope :recent_first, -> { order(RECENT_ORDER) }

  class << self
    def available?
      connection.data_source_exists?(table_name)
    end

    def recent(limit:)
      recent_first.limit(limit).to_a
    end

    def for_workflow(workflow_name, limit:)
      where(
        "workflow_name = :workflow_name OR " \
          "((workflow_name IS NULL OR workflow_name = '') AND task_name = :workflow_name)",
        workflow_name: workflow_name
      ).recent_first.limit(limit).to_a
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
    suffix = queue_name.to_s.delete_prefix("centaur_workflows").delete_prefix("_")
    suffix.presence&.tr("_", " ") || "default"
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
end
