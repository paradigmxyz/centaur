class AddRoleToSlackChannelPermissions < ActiveRecord::Migration[8.1]
  def change
    change_column_null :slack_channel_permissions, :principal_id, true
    add_reference :slack_channel_permissions, :role, null: true, foreign_key: true, index: false

    remove_index :slack_channel_permissions, %i[principal_id channel_id], unique: true
    add_index :slack_channel_permissions,
              %i[principal_id channel_id],
              unique: true,
              where: "principal_id IS NOT NULL",
              name: "idx_slack_permissions_unique_principal_channel"
    add_index :slack_channel_permissions,
              %i[role_id channel_id],
              unique: true,
              where: "role_id IS NOT NULL",
              name: "idx_slack_permissions_unique_role_channel"
    add_check_constraint :slack_channel_permissions,
                         "(principal_id IS NOT NULL) <> (role_id IS NOT NULL)",
                         name: "slack_channel_permissions_exactly_one_grantee"
  end
end
