class AuthoredWorkflowRunJob < ApplicationJob
  MAX_ATTEMPTS = 3

  queue_as :default

  retry_on CentaurApiClient::Error, wait: :polynomially_longer, attempts: 5
  discard_on ActiveRecord::RecordNotFound

  class_attribute :client_factory, default: -> { CentaurApiClient.new }

  def perform(workflow_id, scheduled_for = Time.current.iso8601)
    workflow = AuthoredWorkflow.find(workflow_id)
    result = client_factory.call.create_workflow_run(
      workflow_name: AuthoredWorkflow::WORKFLOW_NAME,
      input: workflow.api_input,
      idempotency_key: idempotency_key(workflow, scheduled_for),
      max_attempts: MAX_ATTEMPTS
    )
    workflow.update!(
      last_run_id: result.fetch("run_id"),
      last_run_at: Time.current,
      last_error: nil
    )
  rescue StandardError => e
    workflow&.update_columns(last_error: e.message, updated_at: Time.current)
    raise
  end

  private

  def idempotency_key(workflow, scheduled_for)
    "authored-workflow:#{workflow.id}:#{Time.iso8601(scheduled_for.to_s).utc.iso8601}"
  end
end
