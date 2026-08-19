class AddAlwaysAvailableToOauthApps < ActiveRecord::Migration[8.1]
  def change
    add_column :oauth_apps, :always_available, :boolean, null: false, default: false
  end
end
