class CreatePrincipalAliases < ActiveRecord::Migration[8.1]
  def change
    create_table :principal_aliases do |t|
      t.references :principal, null: false, foreign_key: { on_delete: :cascade }
      t.string :foreign_id, null: false
      t.timestamps
    end
    add_index :principal_aliases, :foreign_id, unique: true
  end
end
