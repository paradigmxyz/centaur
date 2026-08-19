class EnforceGloballyUniqueForeignIds < ActiveRecord::Migration[8.1]
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
    validate_no_foreign_id_collisions!

    RESOURCE_TABLES.each do |table|
      add_index table, :foreign_id, unique: true
    end
  end

  def down
    RESOURCE_TABLES.reverse_each do |table|
      remove_index table, :foreign_id
    end
  end

  private

  def validate_no_foreign_id_collisions!
    collisions = RESOURCE_TABLES.flat_map do |table|
      select_all(<<~SQL).map do |row|
        SELECT foreign_id,
               COUNT(*) AS record_count,
               STRING_AGG(namespace, ', ' ORDER BY namespace) AS namespaces
        FROM #{table}
        WHERE foreign_id IS NOT NULL
        GROUP BY foreign_id
        HAVING COUNT(*) > 1
        ORDER BY foreign_id
      SQL
        "- #{table}.foreign_id #{row.fetch("foreign_id").inspect} is used " \
          "#{row.fetch("record_count")} times across namespaces: #{row.fetch("namespaces")}"
      end
    end
    return if collisions.empty?

    raise ActiveRecord::MigrationError, <<~MESSAGE
      Cannot enforce globally unique foreign IDs because collisions were found:
      #{collisions.join("\n")}

      Rename or remove the colliding foreign IDs, then retry this migration.
    MESSAGE
  end
end
