class CreateSystemSettings < ActiveRecord::Migration[8.1]
  def change
    create_table :system_settings do |t|
      t.boolean :singleton, null: false, default: true
      t.string :default_sandbox_repo_cache, null: false, default: "all"
      t.boolean :default_sandbox_observability_enabled, null: false, default: true
      t.boolean :default_sandbox_api_server_enabled, null: false, default: true

      t.timestamps
    end

    add_index :system_settings, :singleton, unique: true
  end
end
