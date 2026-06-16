class AddSettingsToPgDsnSecrets < ActiveRecord::Migration[8.1]
  # Pinned session settings (GUCs) the proxy SETs at session start for this
  # upstream, before the SET ROLE. An ordered array of { name, value } objects;
  # defaults to empty so existing upstreams carry no settings.
  def change
    add_column :pg_dsn_secrets, :settings, :jsonb, default: [], null: false
  end
end
