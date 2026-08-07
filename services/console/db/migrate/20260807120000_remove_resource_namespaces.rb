class RemoveResourceNamespaces < ActiveRecord::Migration[8.1]
  RESOURCE_TABLES = %i[
    aws_auth_secrets
    broker_credentials
    gcp_auth_secrets
    gcp_id_token_secrets
    hmac_secrets
    oauth_token_secrets
    pg_dsn_secrets
    principals
    roles
    static_secrets
  ].freeze

  def up
    lock_namespace_tables!
    remove_token_broker_namespaces!
    replace_namespace_principal_fields!

    RESOURCE_TABLES.each do |table|
      remove_index table, name: "index_#{table}_on_namespace_and_foreign_id"
      remove_column table, :namespace, :string
    end
    add_global_principal_indexes!
    remove_column :oauth_apps, :credential_namespace, :string
  end

  def down
    raise ActiveRecord::IrreversibleMigration, "resource namespaces cannot be restored"
  end

  private

  def lock_namespace_tables!
    tables = RESOURCE_TABLES + %i[oauth_apps secret_sources]
    execute "LOCK TABLE #{tables.join(", ")} IN ACCESS EXCLUSIVE MODE"
  end

  def remove_token_broker_namespaces!
    execute <<~SQL
      UPDATE secret_sources
      SET config = config - 'credential_namespace'
      WHERE source_type = 'token_broker'
        AND config ? 'credential_namespace'
    SQL
  end

  def replace_namespace_principal_fields!
    execute <<~SQL
      UPDATE pg_dsn_secrets
      SET settings = (
        SELECT jsonb_agg(
          CASE
            WHEN setting->'value_from'->>'principal_field' = 'namespace'
              THEN (setting - 'value_from') || jsonb_build_object('value', 'default')
            ELSE setting
          END
          ORDER BY position
        ) AS settings
        FROM jsonb_array_elements(pg_dsn_secrets.settings)
          WITH ORDINALITY AS entries(setting, position)
      )
      WHERE EXISTS (
        SELECT 1
        FROM jsonb_array_elements(pg_dsn_secrets.settings) setting
        WHERE setting->'value_from'->>'principal_field' = 'namespace'
      )
    SQL
  end

  def add_global_principal_indexes!
    %i[
      console_user_email
      console_user_id
      kind
      slack_channel_id
      slack_email
      slack_team_id
      slack_user_id
    ].each do |column|
      add_index :principals, column
    end
  end
end
