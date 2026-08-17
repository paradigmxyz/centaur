class AddApiJwtCapabilitiesToPrincipals < ActiveRecord::Migration[8.1]
  def change
    add_column :principals, :sandbox_sessions_read_enabled, :boolean, null: false, default: false
    add_column :principals, :sandbox_workflows_read_enabled, :boolean, null: false, default: false
    add_column :principals, :sandbox_workflows_write_enabled, :boolean, null: false, default: false

    reversible do |direction|
      direction.up do
        execute <<~SQL.squish
          UPDATE principals
          SET sandbox_sessions_read_enabled = sandbox_api_server_enabled
        SQL
      end
    end

    add_column :system_settings, :default_sandbox_sessions_read_enabled, :boolean, null: false, default: false
    add_column :system_settings, :default_sandbox_workflows_read_enabled, :boolean, null: false, default: false
    add_column :system_settings, :default_sandbox_workflows_write_enabled, :boolean, null: false, default: false

    reversible do |direction|
      direction.up do
        execute <<~SQL.squish
          UPDATE system_settings
          SET default_sandbox_sessions_read_enabled = default_sandbox_api_server_enabled
        SQL
      end
    end
  end
end
