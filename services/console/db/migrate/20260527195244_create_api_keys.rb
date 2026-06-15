class CreateApiKeys < ActiveRecord::Migration[8.1]
  def change
    create_table :api_keys do |t|
      t.references :user, null: false, foreign_key: true
      t.string :name, null: false
      t.string :token_hash, null: false

      t.timestamps
    end
    add_index :api_keys, :token_hash, unique: true
  end
end
