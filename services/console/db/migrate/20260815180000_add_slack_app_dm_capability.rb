class AddSlackAppDmCapability < ActiveRecord::Migration[8.1]
  def change
    add_column :principals, :sandbox_slack_app_dm_enabled, :boolean, null: false, default: false
    add_column :system_settings, :default_sandbox_slack_app_dm_enabled, :boolean, null: false, default: false
  end
end
