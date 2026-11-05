class CreateApiClients < ActiveRecord::Migration[8.1]
  def change
    create_table :api_clients do |t|
      t.string :name, null: false
      t.string :secret_digest, null: false
      t.jsonb :scopes, null: false, default: []
      t.integer :max_concurrent_sessions
      t.datetime :disabled_at
      t.references :created_by, null: false, foreign_key: { to_table: :users }

      t.timestamps
    end
    add_index :api_clients, :secret_digest, unique: true

    create_table :api_client_principals do |t|
      t.references :api_client, null: false, foreign_key: { on_delete: :cascade }
      t.references :principal, null: false, foreign_key: { on_delete: :cascade }
      t.boolean :default, null: false, default: false

      t.timestamps
    end
    add_index :api_client_principals, %i[api_client_id principal_id], unique: true
    add_index :api_client_principals, :api_client_id, unique: true, where: '"default"',
              name: "index_api_client_principals_on_default"
  end
end
