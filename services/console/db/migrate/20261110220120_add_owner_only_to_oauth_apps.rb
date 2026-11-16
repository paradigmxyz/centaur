class AddOwnerOnlyToOauthApps < ActiveRecord::Migration[8.1]
  def change
    # Owner-only apps mint one credential per Console user, delivered only to
    # that user's own principals (see OauthApp#owner_only).
    add_column :oauth_apps, :owner_only, :boolean, default: false, null: false
  end
end
