# api-rs now runs scheduled tasks. This job only drains runs Console enqueued
# before that change, handing each occurrence to api-rs under the same
# idempotency key. Remove it once no such jobs remain.
class ScheduledTaskRunJob < ApplicationJob
  queue_as :default

  retry_on CentaurApiClient::Error, wait: :polynomially_longer, attempts: 5
  discard_on ActiveRecord::RecordNotFound

  def perform(task_id, scheduled_for = Time.current.iso8601)
    task = ScheduledTask.find(task_id)
    return unless task.enabled?

    ScheduledTaskRunner.call(task, scheduled_for: Time.iso8601(scheduled_for.to_s))
  rescue CentaurApiClient::Error => e
    # api-rs recorded why the task cannot run; retrying will not change it.
    raise unless e.status == 422
  end
end
