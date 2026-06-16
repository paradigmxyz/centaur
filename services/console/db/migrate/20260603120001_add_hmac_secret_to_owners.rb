class AddHmacSecretToOwners < ActiveRecord::Migration[8.1]
  def change
    # An hmac_sign credential owns its HMAC-key sources (1:many, disambiguated by
    # role like oauth_token) and its request rules, and can be granted directly.
    add_reference :secret_sources, :hmac_secret, null: true, foreign_key: true
    add_reference :request_rules, :hmac_secret, null: true, foreign_key: true
    add_reference :grants, :hmac_secret, null: true, foreign_key: true

    # One source per (entry, role, kind), mirroring the oauth_token owner index.
    add_index :secret_sources, [ :hmac_secret_id, :role, :role_kind ],
              unique: true, name: "index_secret_sources_on_hmac_owner_and_role"
  end
end
