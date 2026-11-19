# Runs a scheduled task now. api-rs runs tasks on their schedules and on
# demand; Console only provisions the author's principal it runs as.
class ScheduledTaskRunner
  class_attribute :client_factory, default: -> { CentaurApiClient.new }

  def self.call(task, scheduled_for: nil)
    ConsoleUserPrincipalProvisioner.call(task.author) if task.author
    client_factory.call.run_scheduled_task(task.oid, scheduled_for: scheduled_for)
  end
end
