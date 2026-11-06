class AddRoleToOauthApps < ActiveRecord::Migration[8.1]
  def change
    add_reference :oauth_apps, :role, foreign_key: true
  end
end
