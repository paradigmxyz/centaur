//! The brain's own schema migrations, separate from api-rs's.
//!
//! They create the `nanocode` schema and the least-privileged `centaur_brain`
//! role the service runs as; nanocode creates its tables inside that schema
//! at runtime. They run with the database owner's credentials, which the
//! long-running service never receives.

use sqlx::{Connection, Executor, PgConnection, migrate::Migrator};

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Holds the migrations' tracking table. sqlx names that table without a
/// schema, so this keeps it apart from api-rs's in `public`, and outside
/// `nanocode`, where the brain role could change it.
const TRACKING_SCHEMA: &str = "centaur_brain_migrations";

pub async fn run(database_url: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut connection = PgConnection::connect(database_url).await?;
    connection
        .execute(format!("create schema if not exists {TRACKING_SCHEMA}").as_str())
        .await?;
    connection
        .execute(format!("set search_path to {TRACKING_SCHEMA}").as_str())
        .await?;
    MIGRATOR.run(&mut connection).await?;
    connection.close().await?;
    Ok(())
}
