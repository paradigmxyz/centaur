class AllowScheduledTasksWithoutSchedule < ActiveRecord::Migration[8.1]
  def change
    change_column_null :scheduled_tasks, :cron_expression, true
  end
end
