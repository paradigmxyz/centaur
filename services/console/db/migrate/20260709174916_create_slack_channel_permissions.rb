class CreateSlackChannelPermissions < ActiveRecord::Migration[8.1]
  def change
    create_table :slack_channel_permissions do |t|
      t.references :principal, null: false, foreign_key: true
      t.string :channel_id, null: false
      t.string :channel_name
      t.boolean :upload_enabled, null: false, default: false
      t.boolean :download_enabled, null: false, default: false
      t.boolean :history_enabled, null: false, default: false

      t.timestamps
    end

    add_index :slack_channel_permissions, %i[principal_id channel_id], unique: true
  end
end
