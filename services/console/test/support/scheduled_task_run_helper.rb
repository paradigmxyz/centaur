# Records the run-now requests Console sends api-rs, so tests need no API.
#
#   assert_scheduled_task_runs(1) { post run_api_v1_console_scheduled_task_path(task.oid) }
module ScheduledTaskRunHelper
  class FakeClient
    attr_reader :runs

    def initialize(error: nil)
      @error = error
      @runs = []
    end

    def run_scheduled_task(task_oid, scheduled_for: nil)
      raise @error if @error

      @runs << { task_oid: task_oid, scheduled_for: scheduled_for }
      { "ok" => true, "run_id" => "run-123" }
    end
  end

  def with_scheduled_task_client(client = FakeClient.new)
    original = ScheduledTaskRunner.client_factory
    ScheduledTaskRunner.client_factory = -> { client }
    yield client
  ensure
    ScheduledTaskRunner.client_factory = original
  end

  def assert_scheduled_task_runs(count, &block)
    with_scheduled_task_client do |client|
      block.call
      assert_equal count, client.runs.length, "expected #{count} scheduled task runs"
    end
  end
end

ActiveSupport.on_load(:active_support_test_case) { include ScheduledTaskRunHelper }
