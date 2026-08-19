class RemoveChannelNameFromSlackChannelPermissions < ActiveRecord::Migration[8.1]
  def change
    remove_column :slack_channel_permissions, :channel_name, :string
  end
end
