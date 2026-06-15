class AddGrantablesToGrants < ActiveRecord::Migration[8.1]
  def change
    # A grant now points at exactly one of three grantable credential types,
    # enforced in the model. static_secret_id is no longer mandatory.
    change_column_null :grants, :static_secret_id, true

    add_reference :grants, :gcp_auth_secret, null: true, foreign_key: true
    add_reference :grants, :oauth_token_secret, null: true, foreign_key: true
  end
end
