class AddProviderConfigToOauthApps < ActiveRecord::Migration[8.1]
  def change
    add_column :oauth_apps, :provider_config, :jsonb, null: false, default: {}
  end
end
