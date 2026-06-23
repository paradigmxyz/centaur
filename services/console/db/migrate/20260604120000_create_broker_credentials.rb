class CreateBrokerCredentials < ActiveRecord::Migration[8.1]
  def change
    create_table :broker_credentials do |t|
      # Identity / config (operator-authored).
      t.string :namespace, null: false, default: "default"
      t.string :foreign_id
      t.string :name
      t.string :description
      t.jsonb :labels, null: false, default: {}
      t.string :token_endpoint, null: false
      t.jsonb :scopes, null: false, default: []

      # OAuth client credentials, resolved by control itself. client_id is not
      # secret (it travels in OAuth requests); client_secret and any custom
      # token-endpoint headers are encrypted at the model layer.
      t.string :client_id
      t.text :client_secret
      t.text :token_endpoint_headers

      # Timing knobs. Defaults mirror iron-token-broker's config.go applyDefaults.
      t.integer :early_refresh_slack_seconds, null: false, default: 300      # 5m
      t.float :early_refresh_fraction, null: false, default: 0.2
      t.integer :max_refresh_interval_seconds, null: false, default: 86_400  # 24h
      t.integer :refresh_timeout_seconds, null: false, default: 30

      # Rotating blob (machine-authored). access_token/refresh_token are
      # encrypted at the model layer; the columns are plain text ciphertext.
      t.text :access_token
      t.text :refresh_token
      t.datetime :expires_at
      t.datetime :last_refresh

      # Scheduler / liveness state.
      t.datetime :next_attempt_at
      t.integer :failure_count, null: false, default: 0
      t.boolean :dead, null: false, default: false
      t.string :dead_reason

      t.references :created_by, null: false, foreign_key: { to_table: :users }

      t.timestamps
    end

    add_index :broker_credentials, [ :namespace, :foreign_id ], unique: true
    add_index :broker_credentials, :labels, using: :gin
    # The poll job scans for credentials whose next refresh is due.
    add_index :broker_credentials, :next_attempt_at
  end
end
