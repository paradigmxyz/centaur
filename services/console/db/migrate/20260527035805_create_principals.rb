class CreatePrincipals < ActiveRecord::Migration[8.1]
  def change
    create_table :principals do |t|
      t.string :namespace, null: false
      t.string :foreign_id, null: false
      t.jsonb :labels, null: false, default: {}

      t.timestamps
    end

    add_index :principals, [ :namespace, :foreign_id ], unique: true
    add_index :principals, :labels, using: :gin
  end
end
