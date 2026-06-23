class AddRoleToGrants < ActiveRecord::Migration[8.1]
  def change
    # A grant is now owned by exactly one grantee: a principal or a role,
    # enforced in the model. principal_id is therefore no longer mandatory.
    add_reference :grants, :role, null: true, foreign_key: true
    change_column_null :grants, :principal_id, true
  end
end
