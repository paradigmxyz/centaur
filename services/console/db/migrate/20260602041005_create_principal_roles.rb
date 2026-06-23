class CreatePrincipalRoles < ActiveRecord::Migration[8.1]
  def change
    create_table :principal_roles do |t|
      t.references :principal, null: false, foreign_key: true
      t.references :role, null: false, foreign_key: true

      t.timestamps
    end

    add_index :principal_roles, [ :principal_id, :role_id ], unique: true
  end
end
