class CreateSlackDmSyncCursors < ActiveRecord::Migration[8.1]
  def change
    create_table :slack_dm_sync_cursors do |t|
      t.string :oauth_app_slug, null: false
      t.bigint :next_credential_id
      t.string :next_conversation_id
      t.datetime :not_before

      t.timestamps
    end

    add_index :slack_dm_sync_cursors, :oauth_app_slug, unique: true
  end
end
