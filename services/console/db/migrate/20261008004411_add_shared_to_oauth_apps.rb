class AddSharedToOauthApps < ActiveRecord::Migration[8.1]
  def change
    add_column :oauth_apps, :shared, :boolean, null: false, default: false
  end
end
