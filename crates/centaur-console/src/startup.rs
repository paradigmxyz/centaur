//! Startup database preparation: connect to the console database, creating it
//! when it does not exist yet (the first `serve`/`worker`/`migrate` on an
//! external Postgres, or a fresh local one).

use centaur_console_api::env::Env;
use sqlx::{Connection, Executor, PgConnection, PgPool};
use tracing::{debug, info};

use crate::config;

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
}

/// Connects, creating the database first when it is missing.
pub async fn prepare_database(env: &Env) -> Result<PgPool, StartupError> {
    let options = config::primary_database(env)?;
    match config::pool_options(env)
        .connect_with(options.clone())
        .await
    {
        Ok(pool) => Ok(pool),
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("3D000") => {
            let name = options
                .get_database()
                .unwrap_or("iron_control_production")
                .to_owned();
            info!(database = %name, "creating the console database");
            let mut admin =
                PgConnection::connect_with(&options.clone().database("postgres")).await?;
            create_database(&mut admin, &name).await?;
            Ok(config::pool_options(env).connect_with(options).await?)
        }
        Err(error) => Err(error.into()),
    }
}

/// Creates `name` unless it already exists. `serve` and `worker` start
/// together on a fresh database and both find it missing, so the loser of the
/// `CREATE DATABASE` race fails: `42P04` (already exists) when the winner
/// committed before its check, or `23505` on `pg_database_datname_index` when
/// both checks pass and the inserts collide. The database it wanted now
/// exists either way, which is success, not a startup failure.
pub async fn create_database(admin: &mut PgConnection, name: &str) -> Result<(), sqlx::Error> {
    let statement = format!("CREATE DATABASE \"{}\"", name.replace('"', "\"\""));
    match admin.execute(statement.as_str()).await {
        Ok(_) => Ok(()),
        Err(error) if created_by_another_process(&error) => {
            debug!(database = %name, "another process created the console database");
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Whether `CREATE DATABASE` failed only because the database now exists.
fn created_by_another_process(error: &sqlx::Error) -> bool {
    let sqlx::Error::Database(error) = error else {
        return false;
    };
    match error.code().as_deref() {
        Some("42P04") => true,
        // `pg_database`'s name index: the existence check and the insert are
        // not atomic, so two concurrent creators can both reach the insert.
        Some("23505") => error.constraint() == Some("pg_database_datname_index"),
        _ => false,
    }
}
