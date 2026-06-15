class CreateStaticSecrets < ActiveRecord::Migration[8.1]
  def change
    create_table :static_secrets do |t|
      t.string :namespace, null: false
      t.string :name, null: false
      t.string :description
      t.jsonb :labels, null: false, default: {}
      t.jsonb :inject_config
      t.jsonb :replace_config

      t.timestamps
    end

    add_index :static_secrets, [ :namespace, :name ], unique: true
    add_index :static_secrets, :labels, using: :gin
  end
end
