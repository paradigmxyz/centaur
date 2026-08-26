class ScheduledTaskRunJob < ApplicationJob
  MAX_ATTEMPTS = 3

  queue_as :default

  retry_on CentaurApiClient::Error, wait: :polynomially_longer, attempts: 5
  discard_on ActiveRecord::RecordNotFound

  class_attribute :client_factory, default: -> { CentaurApiClient.new }

  def perform(task_id, scheduled_for = Time.current.iso8601)
    task = ScheduledTask.find(task_id)
    result = client_factory.call.create_workflow_run(
      workflow_name: ScheduledTask::WORKFLOW_NAME,
      input: task.api_input,
      idempotency_key: idempotency_key(task, scheduled_for),
      max_attempts: MAX_ATTEMPTS
    )
    task.update!(
      last_run_id: result.fetch("run_id"),
      last_run_at: Time.current,
      last_error: nil
    )
  rescue StandardError => e
    task&.update_columns(last_error: e.message, updated_at: Time.current)
    raise
  end

  private

  def idempotency_key(task, scheduled_for)
    "scheduled-task:#{task.id}:#{Time.iso8601(scheduled_for.to_s).utc.iso8601}"
  end
end
