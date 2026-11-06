class AddChannelOwnerToScheduledTasks < ActiveRecord::Migration[8.1]
  def change
    add_reference :scheduled_tasks, :principal, foreign_key: { on_delete: :cascade }, index: false
    add_index :scheduled_tasks, %i[principal_id name], unique: true
    change_column_null :scheduled_tasks, :author_id, true
    add_check_constraint :scheduled_tasks,
                         "(author_id IS NULL) <> (principal_id IS NULL)",
                         name: "scheduled_tasks_exactly_one_owner"
  end
end
