class Console::WorkflowsController < ApplicationController
  layout "console"
  before_action :require_admin

  WORKFLOW_LIMIT = 200
  WORKFLOW_HISTORY_LIMIT = 1_000

  def index
    @workflow_db_unavailable = false
    @workflow_runs = []

    unless CentaurWorkflowRun.available?
      @workflow_db_unavailable = true
      return
    end

    @workflow_runs = CentaurWorkflowRun.recent(limit: WORKFLOW_LIMIT)
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    Rails.logger.warn("console_workflows_load_failed error=#{e.class}: #{e.message}")
    @workflow_db_unavailable = true
    @workflow_runs = []
  end

  def show
    @workflow_db_unavailable = false
    @workflow_name = params[:id].to_s
    @workflow_runs = []

    unless CentaurWorkflowRun.available?
      @workflow_db_unavailable = true
      return
    end

    @workflow_runs = CentaurWorkflowRun.for_workflow(
      @workflow_name,
      limit: WORKFLOW_HISTORY_LIMIT
    )
    @latest_run = @workflow_runs.first
    response.status = :not_found if @latest_run.blank?
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    Rails.logger.warn("console_workflow_load_failed workflow=#{@workflow_name} error=#{e.class}: #{e.message}")
    @workflow_db_unavailable = true
    @workflow_runs = []
    @latest_run = nil
  end
end
