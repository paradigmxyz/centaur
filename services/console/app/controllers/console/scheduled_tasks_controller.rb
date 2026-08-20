class Console::ScheduledTasksController < ApplicationController
  layout "console"
  before_action :require_admin
  before_action :set_task, only: %i[edit update destroy run]

  def index
    @tasks = ScheduledTask.includes(:author, :principal).order(:name, :id)
    @slack_destination_names = slack_destination_names
  end

  def new
    @task = current_user.scheduled_tasks.new(
      cron_expression: ScheduledTask::SCHEDULE_PRESETS.fetch("daily"),
      enabled: true
    )
    prepare_form
  end

  def create
    @task = current_user.scheduled_tasks.new
    if @task.update(task_attributes)
      redirect_to console_scheduled_tasks_path, notice: "Task created."
    else
      prepare_form
      render :new, status: :unprocessable_entity
    end
  end

  def edit
    prepare_form
  end

  def update
    if @task.update(task_attributes)
      redirect_to console_scheduled_tasks_path, notice: "Task saved."
    else
      prepare_form
      render :edit, status: :unprocessable_entity
    end
  end

  def destroy
    @task.destroy!
    redirect_to console_scheduled_tasks_path, notice: "Task deleted."
  end

  def run
    ScheduledTaskRunJob.perform_later(@task.id, Time.current.iso8601)
    redirect_to console_scheduled_tasks_path, notice: "Task queued."
  end

  private

  def set_task
    @task = ScheduledTask.find_by_oid!(params[:id])
  end

  def task_attributes
    attributes = task_params
    attributes.except(:schedule_preset, :principal_oid).merge(
      principal: resolve_principal!(attributes[:principal_oid]),
      cron_expression: ScheduledTask.cron_for(
        attributes[:schedule_preset],
        attributes[:cron_expression]
      )
    )
  end

  def resolve_principal!(oid)
    return if oid.blank?

    Principal.where.not(foreign_id: nil).find_by_oid!(oid)
  end

  def task_params
    params.require(:scheduled_task).permit(
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

  def slack_destination_names
    names = SlackBotChannel.pluck(:channel_id, :name).to_h { |channel_id, name| [ channel_id, "##{name}" ] }
    @tasks.each do |task|
      user_id = SlackDeliveryPolicy.new(task.author).direct_message_user_id
      next unless task.delivery_channel == user_id

      names[user_id] = task.author == current_user ? "Direct message to you" : "Direct message to #{task.author.email}"
    end
    names
  rescue StandardError => e
    Rails.logger.warn("scheduled_task_slack_channels_load_failed error=#{e.class}: #{e.message}")
    {}
  end
end
