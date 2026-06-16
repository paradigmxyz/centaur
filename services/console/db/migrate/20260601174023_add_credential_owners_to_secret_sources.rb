class AddCredentialOwnersToSecretSources < ActiveRecord::Migration[8.1]
  def change
    # A SecretSource hangs off exactly one owner. static_secret already exists;
    # gcp_auth_secret is the (1:1) keyfile, oauth_token_secret holds an entry's
    # credential fields and token-endpoint headers (1:many, disambiguated by role).
    add_reference :secret_sources, :gcp_auth_secret, null: true, foreign_key: true, index: { unique: true }
    add_reference :secret_sources, :oauth_token_secret, null: true, foreign_key: true

    # role names the slot a source fills within an oauth_token entry; role_kind
    # disambiguates whether that name is a credential field (client_id, ...) or a
    # token-endpoint header. Both are null for non-oauth owners.
    add_column :secret_sources, :role, :string
    add_column :secret_sources, :role_kind, :string

    # One source per (entry, role, kind) — a credential field and a header may
    # share a name without colliding.
    add_index :secret_sources, [ :oauth_token_secret_id, :role, :role_kind ],
              unique: true, name: "index_secret_sources_on_oauth_owner_and_role"
  end
end
