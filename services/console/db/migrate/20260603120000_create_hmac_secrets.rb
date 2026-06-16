class CreateHmacSecrets < ActiveRecord::Migration[8.1]
  def change
    create_table :hmac_secrets do |t|
      t.string :namespace, null: false, default: "default"
      t.string :foreign_id
      t.string :name
      t.string :description
      t.jsonb :labels, null: false, default: {}
      t.string :timestamp_format
      t.string :signature_algorithm
      t.string :signature_key_encoding
      t.string :signature_output_encoding
      t.text :signature_message
      t.jsonb :headers, null: false, default: []
      t.boolean :allow_chunked_body, null: false, default: false
      t.references :created_by, null: false, foreign_key: { to_table: :users }

      t.timestamps
    end

    add_index :hmac_secrets, [ :namespace, :foreign_id ], unique: true
    add_index :hmac_secrets, :labels, using: :gin
  end
end
