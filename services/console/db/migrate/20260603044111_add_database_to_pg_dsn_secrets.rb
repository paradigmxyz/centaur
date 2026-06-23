class AddDatabaseToPgDsnSecrets < ActiveRecord::Migration[8.1]
  def change
    add_column :pg_dsn_secrets, :database, :string
  end
end
