class CreatePrincipalSyncConfigSnapshots < ActiveRecord::Migration[8.1]
  def change
    add_column :principals, :sync_config_cache_version, :bigint, null: false, default: 0

    create_table :principal_sync_config_snapshots do |t|
      t.references :principal, null: false, foreign_key: true
      t.bigint :principal_cache_version, null: false
      t.text :payload, null: false

      t.timestamps
    end

    add_index :principal_sync_config_snapshots, [ :principal_id, :principal_cache_version ],
              unique: true, name: "idx_principal_sync_snapshots_on_principal_version"
    add_index :principal_sync_config_snapshots, :updated_at
  end
end
