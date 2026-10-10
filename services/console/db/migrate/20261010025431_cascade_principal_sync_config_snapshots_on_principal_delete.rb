class CascadePrincipalSyncConfigSnapshotsOnPrincipalDelete < ActiveRecord::Migration[8.1]
  # Console no longer manages snapshot rows, so leftover rows must not block
  # principal deletion until the table is dropped.
  def change
    remove_foreign_key :principal_sync_config_snapshots, :principals
    add_foreign_key :principal_sync_config_snapshots, :principals, on_delete: :cascade
  end
end
