class CreateScheduledTasks < ActiveRecord::Migration[8.1]
  def change
    create_table :scheduled_tasks do |t|
      t.string :name, null: false
      t.text :prompt, null: false
      t.references :author, null: false, foreign_key: { to_table: :users }
      t.string :delivery_channel, null: false
      t.string :cron_expression, null: false
      t.string :timezone, null: false, default: "America/Los_Angeles"
      t.boolean :enabled, null: false, default: true
      t.datetime :next_run_at
      t.datetime :last_enqueued_at
      t.string :last_run_id
      t.datetime :last_run_at
      t.text :last_error

      t.timestamps

      t.index [ :author_id, :name ], unique: true
      t.index [ :enabled, :next_run_at ]
    end
  end
end
