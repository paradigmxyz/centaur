# Rails schema-qualifies extensions installed outside the current schema. That
# produces `CREATE EXTENSION pg_search SCHEMA paradedb`, which cannot create the
# extension-owned schema when replaying schema.rb. Keep pg_search unqualified so
# PostgreSQL can install it into its configured schema.
module ParadeDbSchemaDump
  def extensions
    super.map { |extension| extension == "paradedb.pg_search" ? "pg_search" : extension }
  end
end

ActiveSupport.on_load(:active_record_postgresqladapter) do
  prepend ParadeDbSchemaDump
end
