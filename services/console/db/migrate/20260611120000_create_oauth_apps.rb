class CreateOauthApps < ActiveRecord::Migration[8.1]
  def change
    create_table :oauth_apps do |t|
      # Identity. The slug is the app's whole identity: a globally-unique,
      # URL-safe name that both addresses the app in the API and names its
      # well-known consent links (/oauth/<slug>/start and /callback). A team
      # member recognizes it ("google"); no namespace/foreign_id/name needed.
      t.string :slug, null: false
      t.string :description
      t.jsonb :labels, null: false, default: {}

      # Which provider strategy drives this app's consent flows ("google").
      t.string :provider, null: false

      # OAuth client. client_id is not secret; client_secret is encrypted at the
      # model layer.
      t.string :client_id, null: false
      t.text :client_secret

      # Scopes the start endpoint requests. When the flow omits its optional
      # `scopes` param, all of these are requested.
      t.jsonb :allowed_scopes, null: false, default: []

      # Namespace for broker credentials minted by this app's flows.
      t.string :credential_namespace, null: false, default: "default"

      # Kill switch: a disabled app rejects new start/callback flows but existing
      # credentials keep refreshing.
      t.boolean :enabled, null: false, default: true

      t.references :created_by, null: false, foreign_key: { to_table: :users }
      t.timestamps
    end

    add_index :oauth_apps, :slug, unique: true
    add_index :oauth_apps, :labels, using: :gin
  end
end
