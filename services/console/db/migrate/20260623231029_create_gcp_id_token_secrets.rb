class CreateGcpIdTokenSecrets < ActiveRecord::Migration[8.1]
  def change
    create_table :gcp_id_token_secrets do |t|
      t.string :namespace, null: false, default: "default"
      t.string :foreign_id
      t.string :name
      t.string :description
      t.jsonb :labels, null: false, default: {}
      t.string :audience, null: false
      t.string :header
      t.references :created_by, null: false, foreign_key: { to_table: :users }

      t.timestamps
    end

    add_index :gcp_id_token_secrets, [ :namespace, :foreign_id ], unique: true
    add_index :gcp_id_token_secrets, :labels, using: :gin
  end
end
