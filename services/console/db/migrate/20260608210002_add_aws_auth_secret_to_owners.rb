class AddAwsAuthSecretToOwners < ActiveRecord::Migration[8.1]
  def change
    # An aws_auth credential owns its SigV4 credential sources (access_key_id,
    # secret_access_key, and optional session_token — 1:many, disambiguated by
    # role like oauth_token/hmac) and its request rules, and can be granted
    # directly.
    add_reference :secret_sources, :aws_auth_secret, null: true, foreign_key: true
    add_reference :request_rules, :aws_auth_secret, null: true, foreign_key: true
    add_reference :grants, :aws_auth_secret, null: true, foreign_key: true

    # One source per (entry, role, kind), mirroring the oauth_token/hmac owners.
    add_index :secret_sources, [ :aws_auth_secret_id, :role, :role_kind ],
              unique: true, name: "index_secret_sources_on_aws_owner_and_role"

    # A grantee + aws_auth_secret pair can exist at most once (see DedupeGrants).
    add_index :grants, [ :principal_id, :aws_auth_secret_id ], unique: true,
              where: "principal_id IS NOT NULL AND aws_auth_secret_id IS NOT NULL",
              name: "index_grants_uniq_principal_aws_auth_secret"
    add_index :grants, [ :role_id, :aws_auth_secret_id ], unique: true,
              where: "role_id IS NOT NULL AND aws_auth_secret_id IS NOT NULL",
              name: "index_grants_uniq_role_aws_auth_secret"
  end
end
