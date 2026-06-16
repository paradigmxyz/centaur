class AddSsoFieldsToUsers < ActiveRecord::Migration[8.1]
  # Adds the console-SSO + approval columns. New users land `pending` and cannot
  # use the console until an admin approves them; `admin` gates the approval
  # screen. `name` carries the IdP display name. password_digest becomes nullable
  # because SSO-only users have no password.
  def up
    add_column :users, :status, :string, null: false, default: "pending"
    add_column :users, :admin, :boolean, null: false, default: false
    add_column :users, :name, :string
    add_column :users, :approved_at, :datetime
    add_reference :users, :approved_by, foreign_key: { to_table: :users }

    change_column_null :users, :password_digest, true

    # Existing operators predate SSO + approval and are the current admins; keep
    # them working rather than locking everyone out behind the new gate.
    execute "UPDATE users SET status = 'active', admin = true"
  end

  def down
    remove_reference :users, :approved_by, foreign_key: { to_table: :users }
    remove_column :users, :approved_at
    remove_column :users, :name
    remove_column :users, :admin
    remove_column :users, :status
    change_column_null :users, :password_digest, false
  end
end
