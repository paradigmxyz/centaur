class AddFeishuIdentityAttributesToUserIdentities < ActiveRecord::Migration[8.1]
  def change
    add_column :user_identities, :tenant_key, :string
    add_column :user_identities, :open_id, :string
    add_index :user_identities, [ :provider, :tenant_key, :open_id ], unique: true,
              where: "provider = 'feishu'",
              name: "index_user_identities_on_feishu_delivery_identity"
  end
end
