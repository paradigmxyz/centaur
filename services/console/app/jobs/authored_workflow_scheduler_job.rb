class AuthoredWorkflowSchedulerJob < ApplicationJob
  queue_as :default

  BATCH_SIZE = 100

  def perform(now = Time.current)
    AuthoredWorkflow.due(now).order(:next_run_at, :id).limit(BATCH_SIZE).pluck(:id).each do |workflow_id|
      enqueue_due_run(workflow_id, now)
    end
  end

  private

  def enqueue_due_run(workflow_id, now)
    AuthoredWorkflow.transaction do
      workflow = AuthoredWorkflow.lock.find(workflow_id)
      return unless workflow.enabled? && workflow.next_run_at.present? && workflow.next_run_at <= now

      scheduled_for = workflow.next_run_at
      workflow.update!(
        last_enqueued_at: scheduled_for,
        next_run_at: workflow.next_occurrence(after: now)
      )
      queued_job = AuthoredWorkflowRunJob.perform_later(workflow.id, scheduled_for.iso8601)
      raise ActiveJob::EnqueueError, "Authored workflow run was not enqueued" unless queued_job
    end
  rescue ActiveRecord::RecordNotFound
    nil
  end
end
