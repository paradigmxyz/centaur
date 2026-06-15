Rails.application.config.after_initialize do
  next if Rails.env.test?
  next if ConsoleEnv["INITIAL_USER_EMAIL"].to_s.strip.empty?

  begin
    # after_initialize fires on every boot, including the environment load that
    # `db:prepare` performs *before* it applies migrations. Skip until the schema
    # is current so we never load a model (e.g. User's status enum) whose column a
    # pending migration still has to add. The server's own boot, which happens
    # after db:prepare has migrated, runs the bootstrap cleanly.
    next if ActiveRecord::Base.connection_pool.migration_context.needs_migration?

    Iron::Bootstrap.run!
  rescue ActiveRecord::NoDatabaseError, ActiveRecord::ConnectionNotEstablished, ActiveRecord::StatementInvalid
    # DB not provisioned, not reachable, or its schema_migrations table not
    # created yet (e.g. during `db:create`). A later boot will bootstrap.
  end
end
