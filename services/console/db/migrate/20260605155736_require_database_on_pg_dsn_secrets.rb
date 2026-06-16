class RequireDatabaseOnPgDsnSecrets < ActiveRecord::Migration[8.1]
  def change
    change_column_null :pg_dsn_secrets, :database, false
  end
end
