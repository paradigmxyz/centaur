class CreateSlackBotChannels < ActiveRecord::Migration[8.1]
  def change
    create_table :slack_bot_channels do |t|
      t.string :team_id, null: false
      t.string :bot_user_id, null: false
      t.string :channel_id, null: false
      t.string :name, null: false
      t.boolean :private, null: false, default: false
      t.boolean :archived, null: false, default: false
      t.boolean :active, null: false, default: true
      t.text :member_user_ids, array: true, null: false, default: []
      t.datetime :membership_refreshed_at
      t.datetime :membership_last_attempted_at
      t.text :membership_error
      t.datetime :last_seen_at

      t.timestamps
    end

    add_index :slack_bot_channels, %i[team_id channel_id], unique: true
    add_index :slack_bot_channels, %i[team_id active name], name: "index_slack_bot_channels_for_catalog_search"
    add_index :slack_bot_channels, :member_user_ids, using: :gin
  end
end
