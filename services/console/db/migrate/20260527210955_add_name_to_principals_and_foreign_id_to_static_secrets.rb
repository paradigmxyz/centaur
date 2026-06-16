class AddNameToPrincipalsAndForeignIdToStaticSecrets < ActiveRecord::Migration[8.1]
  def change
    add_column :principals, :name, :string
    change_column_null :principals, :namespace, true
    change_column_null :principals, :foreign_id, true

    add_column :static_secrets, :foreign_id, :string
    change_column_null :static_secrets, :namespace, true
    change_column_null :static_secrets, :name, true

    remove_index :static_secrets, name: "index_static_secrets_on_namespace_and_name"
    add_index :static_secrets, [ :namespace, :foreign_id ], unique: true
  end
end
