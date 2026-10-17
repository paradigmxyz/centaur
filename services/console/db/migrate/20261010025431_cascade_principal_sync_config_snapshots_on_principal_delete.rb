class CascadePrincipalSyncConfigSnapshotsOnPrincipalDelete < ActiveRecord::Migration[8.1]
  # Console no longer manages snapshot rows. Nothing reads them, and each one
  # holds an encrypted copy of a principal's resolved credentials, so delete
  # them rather than keep rotated or revoked values at rest. Old pods still
  # serving during a rollout may write a few more; cascading keeps those from
  # blocking principal deletion until the table is dropped.
  def change
    reversible do |direction|
      direction.up { execute "DELETE FROM principal_sync_config_snapshots" }
    end
    remove_foreign_key :principal_sync_config_snapshots, :principals
    add_foreign_key :principal_sync_config_snapshots, :principals, on_delete: :cascade
  end
end
