class CreateRoles < ActiveRecord::Migration[8.1]
  def change
    create_table :roles do |t|
      t.string :namespace, null: false, default: "default"
      t.string :foreign_id
      t.string :name
      t.jsonb :labels, null: false, default: {}
      t.references :created_by, null: false, foreign_key: { to_table: :users }

      t.timestamps
    end

    add_index :roles, [ :namespace, :foreign_id ], unique: true
    add_index :roles, :labels, using: :gin
  end
end
