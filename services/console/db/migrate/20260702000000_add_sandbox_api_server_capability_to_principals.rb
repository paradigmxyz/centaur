class AddSandboxApiServerCapabilityToPrincipals < ActiveRecord::Migration[8.1]
  def change
    add_column :principals, :sandbox_api_server_enabled, :boolean, null: false, default: true
  end
end
