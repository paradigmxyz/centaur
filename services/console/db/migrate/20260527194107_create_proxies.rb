class CreateProxies < ActiveRecord::Migration[8.1]
  def change
    create_table :proxies do |t|
      t.string :name, null: false
      t.references :principal, null: false, foreign_key: true
      t.string :bearer_token_hash, null: false

      t.timestamps
    end
  end
end
