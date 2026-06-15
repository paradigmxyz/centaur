class AddPgDsnSecretToSecretSources < ActiveRecord::Migration[8.1]
  def change
    # The (1:1) upstream DSN source for a PgDsnSecret. Like the static_secret
    # and gcp_auth_secret owners, at most one source per owner.
    add_reference :secret_sources, :pg_dsn_secret, null: true, foreign_key: true, index: { unique: true }
  end
end
