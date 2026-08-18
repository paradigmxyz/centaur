class Console::WorkflowsController < ApplicationController
  layout "console"
  before_action :require_admin

  class_attribute :client_factory, default: -> { CentaurApiClient.new }

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

    load_workflow_api_details
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    Rails.logger.warn("console_workflow_load_failed workflow=#{@workflow_name} error=#{e.class}: #{e.message}")
    @workflow_db_unavailable = true
    @workflow_runs = []
    @latest_run = nil
  end

  # Enqueue a run through the workflows API. Scheduled workflows are started
  # with their registered schedule input so a forced run matches a normal tick.
  def force_start
    workflow_name = params[:id].to_s
    schedule = workflow_schedules_for(workflow_name).first
    result = api_client.create_workflow_run(
      workflow_name: workflow_name,
      input: schedule&.dig("input")
    )
    notice =
      if result["created"] == false
        "A run with this idempotency key is already queued (#{result["run_id"]})."
      else
        "Run queued (#{result["run_id"]})."
      end
    redirect_to console_workflow_path(workflow_name), notice: notice
  rescue StandardError => e
    Rails.logger.warn("console_workflow_force_start_failed workflow=#{workflow_name} error=#{e.class}: #{e.message}")
    redirect_to console_workflow_path(workflow_name), alert: "Could not start workflow: #{e.message}"
  end

  private

  # Best-effort enrichment from the workflows API: the registered schedule
  # (cron/interval, source path for the GitHub link) and the latest run's
  # input/result/failure for debugging. The page renders without any of it
  # when the API is unreachable.
  def load_workflow_api_details
    @workflow_schedules = workflow_schedules_for(@workflow_name)
    @latest_run_detail = fetch_run_detail(@latest_run&.run_id)

    return if @latest_run_detail.blank? && @workflow_schedules.blank?
    return if @latest_run&.display_status == "failed"
    return unless @status_counts["failed"].to_i.positive?

    failed_run = CentaurWorkflowRun.for_workflow(@workflow_name, limit: 1, status: "failed").first
    @latest_failure_detail = fetch_run_detail(failed_run&.run_id)
  end

  def workflow_schedules_for(workflow_name)
    response = api_client.list_workflow_schedules
    Array(response["schedules"]).select do |schedule|
      schedule.is_a?(Hash) && schedule["workflow_name"] == workflow_name
    end
  rescue StandardError => e
    Rails.logger.warn("console_workflow_schedules_failed error=#{e.class}: #{e.message}")
    []
  end

  def fetch_run_detail(run_id)
    return nil if run_id.blank?

    response = api_client.get_workflow_run(run_id)
    detail = response["run"]
    detail.is_a?(Hash) ? detail : nil
  rescue StandardError => e
    Rails.logger.warn("console_workflow_run_detail_failed run=#{run_id} error=#{e.class}: #{e.message}")
    nil
  end

  def api_client
    @api_client ||= self.class.client_factory.call
  end

  def page_param
    page = Integer(params[:page].to_s, 10, exception: false) || 1
    page < 1 ? 1 : page
  end
end
