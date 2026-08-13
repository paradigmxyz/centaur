class RemoveConsoleUserEmailFromPrincipals < ActiveRecord::Migration[8.1]
  def up
    remove_column :principals, :console_user_email, :string
  end

  def down
    add_column :principals, :console_user_email, :string
    add_index :principals, :console_user_email
  end
end
