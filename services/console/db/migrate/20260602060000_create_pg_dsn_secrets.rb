class CreatePgDsnSecrets < ActiveRecord::Migration[8.1]
  def change
    create_table :pg_dsn_secrets do |t|
      t.string :namespace, null: false, default: "default"
      t.string :foreign_id
      t.string :name
      t.string :description
      t.jsonb :labels, null: false, default: {}

      # The credential itself is the upstream DSN, stored as a secret source
      # (see secret_sources.pg_dsn_secret_id). role is an optional SET ROLE
      # applied to the upstream session. The listener and client-auth knobs
      # (listen, client.user/password_env) are deliberately NOT modeled here:
      # they are proxy-host deployment concerns the proxy configures locally and
      # binds to this secret by oid (mirroring how it does not expose the
      # gcp_auth keyfile_path).
      t.string :role

      t.references :created_by, null: false, foreign_key: { to_table: :users }

      t.timestamps
    end

    add_index :pg_dsn_secrets, [ :namespace, :foreign_id ], unique: true
    add_index :pg_dsn_secrets, :labels, using: :gin
  end
end
