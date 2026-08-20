class ScheduledTaskSchedulerJob < ApplicationJob
  queue_as :default

  BATCH_SIZE = 100

  def perform(now = Time.current)
    ScheduledTask.due(now).order(:next_run_at, :id).limit(BATCH_SIZE).pluck(:id).each do |task_id|
      enqueue_due_run(task_id, now)
    end
  end

  private

  def enqueue_due_run(task_id, now)
    ScheduledTask.transaction do
      task = ScheduledTask.lock.find(task_id)
      return unless task.enabled? && task.next_run_at.present? && task.next_run_at <= now

      scheduled_for = task.next_run_at
      task.update!(
        last_enqueued_at: scheduled_for,
        next_run_at: task.next_occurrence(after: now)
      )
      queued_job = ScheduledTaskRunJob.perform_later(task.id, scheduled_for.iso8601)
      raise ActiveJob::EnqueueError, "Scheduled task run was not enqueued" unless queued_job
    end
  rescue ActiveRecord::RecordNotFound
    nil
  end
end
