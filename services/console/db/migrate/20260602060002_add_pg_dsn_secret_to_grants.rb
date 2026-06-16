class AddPgDsnSecretToGrants < ActiveRecord::Migration[8.1]
  def change
    # A fourth grantable credential type: a Postgres upstream the proxy may serve.
    add_reference :grants, :pg_dsn_secret, null: true, foreign_key: true
  end
end
