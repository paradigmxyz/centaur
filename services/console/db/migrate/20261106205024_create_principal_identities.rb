# One principal per person: the foreign ids a person is known by beyond their
# principal's own (their Slack user, their Console account) name the same
# principal, and the per-surface Slack DM and Console-user kinds become one
# person kind.
class CreatePrincipalIdentities < ActiveRecord::Migration[8.1]
  def up
    create_table :principal_identities do |t|
      t.references :principal, null: false, foreign_key: { on_delete: :cascade }
      t.string :foreign_id, null: false
      t.timestamps
    end
    add_index :principal_identities, :foreign_id, unique: true

    execute <<~SQL
      UPDATE principals
      SET kind = 'person',
          sync_config_cache_version = sync_config_cache_version + 1,
          updated_at = CURRENT_TIMESTAMP
      WHERE kind IN ('slack_dm', 'console_user')
    SQL
  end

  def down
    execute <<~SQL
      UPDATE principals
      SET kind = CASE WHEN foreign_id LIKE 'console-user-%' THEN 'console_user' ELSE 'slack_dm' END,
          sync_config_cache_version = sync_config_cache_version + 1,
          updated_at = CURRENT_TIMESTAMP
      WHERE kind = 'person'
    SQL
    drop_table :principal_identities
  end
end
