class Console::WorkflowsController < ApplicationController
  layout "console"
  before_action :require_admin

  PER_PAGE = 50

  def index
    @workflow_db_unavailable = false
    @workflow_runs = []
    @queue_breakdown = {}
    @page = page_param
    @total_pages = 1

    unless CentaurWorkflowRun.available?
      @workflow_db_unavailable = true
      return
    end

    @total_workflows = CentaurWorkflowRun.workflow_count
    @total_pages = [ (@total_workflows.to_f / PER_PAGE).ceil, 1 ].max
    @page = [ @page, @total_pages ].min

    @workflow_runs = CentaurWorkflowRun.latest_per_workflow(
      limit: PER_PAGE,
      offset: (@page - 1) * PER_PAGE
    )
    @queue_breakdown = CentaurWorkflowRun.latest_per_queue(@workflow_runs.map(&:workflow_key))
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    Rails.logger.warn("console_workflows_load_failed error=#{e.class}: #{e.message}")
    @workflow_db_unavailable = true
    @workflow_runs = []
    @queue_breakdown = {}
  end

  def show
    @workflow_db_unavailable = false
    @workflow_name = params[:id].to_s
    @workflow_runs = []
    @status_counts = {}
    @queue_names = []
    @status = params[:status].presence
    @queue = params[:queue].presence
    @page = page_param
    @total_pages = 1

    unless CentaurWorkflowRun.available?
      @workflow_db_unavailable = true
      return
    end

    @latest_run = CentaurWorkflowRun.for_workflow(@workflow_name, limit: 1).first
    if @latest_run.blank?
      response.status = :not_found
      return
    end

    @status_counts = CentaurWorkflowRun.status_counts(@workflow_name)
    @total_runs = @status_counts.values.sum
    @queue_names = CentaurWorkflowRun.queue_names(@workflow_name)

    @filtered_count = CentaurWorkflowRun.run_count(@workflow_name, status: @status, queue: @queue)
    @total_pages = [ (@filtered_count.to_f / PER_PAGE).ceil, 1 ].max
    @page = [ @page, @total_pages ].min

    @workflow_runs = CentaurWorkflowRun.for_workflow(
      @workflow_name,
      limit: PER_PAGE,
      offset: (@page - 1) * PER_PAGE,
      status: @status,
      queue: @queue
    )
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    Rails.logger.warn("console_workflow_load_failed workflow=#{@workflow_name} error=#{e.class}: #{e.message}")
    @workflow_db_unavailable = true
    @workflow_runs = []
    @latest_run = nil
  end

  private

  def page_param
    page = Integer(params[:page].to_s, 10, exception: false) || 1
    page < 1 ? 1 : page
  end
end
