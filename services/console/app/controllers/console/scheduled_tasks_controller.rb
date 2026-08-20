class Console::ScheduledTasksController < ApplicationController
  layout "console"
  before_action :require_admin
  before_action :set_task, only: %i[edit update destroy run]

  def index
    @tasks = current_user.scheduled_tasks.order(:name, :id)
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
    @task = current_user.scheduled_tasks.find_by_oid!(params[:id])
  end

  def task_attributes
    attributes = task_params
    delivery_mode = attributes.delete(:delivery_mode)
    attributes.except(:schedule_preset).merge(
      delivery_channel: delivery_channel_for(delivery_mode, attributes[:delivery_channel]),
      cron_expression: ScheduledTask.cron_for(
        attributes[:schedule_preset],
        attributes[:cron_expression]
      )
    )
  end

  def delivery_channel_for(delivery_mode, channel_id)
    return SlackDeliveryPolicy.new(current_user).direct_message_user_id if delivery_mode == "dm"
    return channel_id if delivery_mode == "channel"

    nil
  end

  def task_params
    params.require(:scheduled_task).permit(
      :name,
      :prompt,
      :delivery_mode,
      :delivery_channel,
      :schedule_preset,
      :cron_expression,
      :enabled
    )
  end

  def prepare_form
    @slack_dm_user_id = SlackDeliveryPolicy.new(current_user).direct_message_user_id
  end

  def slack_destination_names
    names = SlackBotChannel.pluck(:channel_id, :name).to_h { |channel_id, name| [ channel_id, "##{name}" ] }
    @tasks.each do |task|
      user_id = SlackDeliveryPolicy.new(task.author).direct_message_user_id
      next unless task.delivery_channel == user_id

      names[user_id] = "Direct message to you"
    end
    names
  rescue StandardError => e
    Rails.logger.warn("scheduled_task_slack_channels_load_failed error=#{e.class}: #{e.message}")
    {}
  end
end
