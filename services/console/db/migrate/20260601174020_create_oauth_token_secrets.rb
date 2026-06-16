class CreateOauthTokenSecrets < ActiveRecord::Migration[8.1]
  def change
    create_table :oauth_token_secrets do |t|
      t.string :namespace, null: false, default: "default"
      t.string :foreign_id
      t.string :name
      t.string :description
      t.jsonb :labels, null: false, default: {}
      t.string :grant
      t.string :token_endpoint
      t.string :audience
      t.jsonb :scopes, null: false, default: []
      t.string :header
      t.string :value_prefix
      t.references :created_by, null: false, foreign_key: { to_table: :users }

      t.timestamps
    end

    add_index :oauth_token_secrets, [ :namespace, :foreign_id ], unique: true
    add_index :oauth_token_secrets, :labels, using: :gin
  end
end
