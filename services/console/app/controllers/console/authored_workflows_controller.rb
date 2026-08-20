class Console::AuthoredWorkflowsController < ApplicationController
  layout "console"
  before_action :require_admin
  before_action :set_workflow, only: %i[edit update destroy run]

  def new
    @workflow = current_user.authored_workflows.new(
      cron_expression: AuthoredWorkflow::SCHEDULE_PRESETS.fetch("daily"),
      enabled: true
    )
    prepare_form
  end

  def create
    @workflow = current_user.authored_workflows.new
    if save_workflow
      redirect_to console_workflows_path, notice: "Workflow created."
    else
      prepare_form
      render :new, status: :unprocessable_entity
    end
  end

  def edit
    prepare_form
  end

  def update
    if save_workflow
      redirect_to console_workflows_path, notice: "Workflow saved."
    else
      prepare_form
      render :edit, status: :unprocessable_entity
    end
  end

  def destroy
    @workflow.destroy!
    redirect_to console_workflows_path, notice: "Workflow deleted."
  end

  def run
    AuthoredWorkflowRunJob.perform_later(@workflow.id, Time.current.iso8601)
    redirect_to console_workflows_path, notice: "Workflow queued."
  end

  private

  def set_workflow
    @workflow = AuthoredWorkflow.find_by_oid!(params[:id])
  end

  def save_workflow
    principal_oid = workflow_params[:principal_oid]
    principal = resolve_principal(principal_oid)
    unless principal_oid.blank? || principal
      @workflow.errors.add(:principal, "is unavailable")
      return false
    end

    @workflow.update(workflow_params.except(:principal_oid).merge(principal: principal))
  end

  def resolve_principal(oid)
    return if oid.blank?

    Principal.where.not(foreign_id: nil).find_by_oid(oid)
  end

  def workflow_params
    params.require(:authored_workflow).permit(
      :name,
      :prompt,
      :principal_oid,
      :delivery_channel,
      :schedule_preset,
      :cron_expression,
      :enabled
    )
  end

  def prepare_form
    @principals = Principal.where.not(foreign_id: nil).order(:name, :foreign_id)
  end
end
