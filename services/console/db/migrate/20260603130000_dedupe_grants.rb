class DedupeGrants < ActiveRecord::Migration[8.1]
  # The grantable columns a grant can reference; each grant sets exactly one,
  # scoped to exactly one grantee (principal_id or role_id).
  GRANTABLES = %i[
    static_secret_id gcp_auth_secret_id oauth_token_secret_id
    pg_dsn_secret_id hmac_secret_id
  ].freeze

  def up
    # Collapse any pre-existing duplicate grants (same grantee + same grantable),
    # keeping the lowest id, so the unique indexes below can be created. NULLs are
    # compared with IS NOT DISTINCT FROM so the unset columns match.
    execute(<<~SQL)
      DELETE FROM grants a USING grants b
      WHERE a.id > b.id
        AND a.principal_id IS NOT DISTINCT FROM b.principal_id
        AND a.role_id IS NOT DISTINCT FROM b.role_id
        AND a.static_secret_id IS NOT DISTINCT FROM b.static_secret_id
        AND a.gcp_auth_secret_id IS NOT DISTINCT FROM b.gcp_auth_secret_id
        AND a.oauth_token_secret_id IS NOT DISTINCT FROM b.oauth_token_secret_id
        AND a.pg_dsn_secret_id IS NOT DISTINCT FROM b.pg_dsn_secret_id
        AND a.hmac_secret_id IS NOT DISTINCT FROM b.hmac_secret_id;
    SQL

    # One grantee + grantable pair can exist at most once. A grant has exactly one
    # grantee, so each grantable type needs a per-principal and a per-role partial
    # unique index; the NOT NULL predicates keep the index off unrelated rows.
    GRANTABLES.each do |grantable|
      add_index :grants, [ :principal_id, grantable ], unique: true,
                where: "principal_id IS NOT NULL AND #{grantable} IS NOT NULL",
                name: "index_grants_uniq_principal_#{grantable.to_s.delete_suffix("_id")}"
      add_index :grants, [ :role_id, grantable ], unique: true,
                where: "role_id IS NOT NULL AND #{grantable} IS NOT NULL",
                name: "index_grants_uniq_role_#{grantable.to_s.delete_suffix("_id")}"
    end
  end

  def down
    GRANTABLES.each do |grantable|
      remove_index :grants, name: "index_grants_uniq_principal_#{grantable.to_s.delete_suffix("_id")}"
      remove_index :grants, name: "index_grants_uniq_role_#{grantable.to_s.delete_suffix("_id")}"
    end
  end
end
