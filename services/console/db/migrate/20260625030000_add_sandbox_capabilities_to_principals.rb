class AddSandboxCapabilitiesToPrincipals < ActiveRecord::Migration[8.1]
  def change
    add_column :principals, :sandbox_repo_cache_enabled, :boolean, null: false, default: true
    add_column :principals, :sandbox_observability_enabled, :boolean, null: false, default: true
  end
end
