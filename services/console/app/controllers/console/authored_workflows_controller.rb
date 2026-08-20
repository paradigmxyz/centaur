class Console::AuthoredWorkflowsController < ApplicationController
  layout "console"
  before_action :require_admin
  before_action :set_workflow, only: %i[edit update destroy run]

  def new
    @workflow = current_user.authored_workflows.new(
      cron_expression: AuthoredWorkflow::SCHEDULE_PRESETS.fetch("daily"),
      timezone: Time.zone.tzinfo.name,
      enabled: true
    )
    prepare_form
  end

  def create
    @workflow = current_user.authored_workflows.new
    if assign_and_save
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
    if assign_and_save
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

  def assign_and_save
    attributes = workflow_params.to_h.symbolize_keys
    schedule_preset = attributes.delete(:schedule_preset)
    custom_cron = attributes.delete(:cron_expression)
    principal_oid = attributes.delete(:principal_oid)
    @workflow.schedule_preset = schedule_preset
    attributes[:cron_expression] = AuthoredWorkflow.cron_for(schedule_preset, custom_cron)
    @workflow.assign_attributes(attributes)

    principal = resolve_principal(principal_oid)
    unless principal_oid.blank? || principal
      @workflow.errors.add(:principal, "is unavailable")
      return false
    end
    @workflow.principal = principal
    @workflow.save
  end

  def resolve_principal(oid)
    return nil if oid.blank?

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
      :timezone,
      :enabled
    )
  end

  def prepare_form
    @principals = Principal.where.not(foreign_id: nil).order(:name, :foreign_id)
    @timezone_options = ActiveSupport::TimeZone.all.map { |zone| zone.tzinfo.name }.uniq.sort
  end
end
