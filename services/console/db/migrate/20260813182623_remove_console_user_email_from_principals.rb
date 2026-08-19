class RemoveConsoleUserEmailFromPrincipals < ActiveRecord::Migration[8.1]
  def up
    remove_column :principals, :console_user_email, :string
  end

  def down
    add_column :principals, :console_user_email, :string
    execute <<~SQL
      UPDATE principals
      SET console_user_email = users.email
      FROM users
      WHERE principals.console_user_id = users.id
    SQL
    add_index :principals, :console_user_email
  end
end
