use std::str::FromStr;

use anyhow::{Context, Result};
use sqlx::{
    Connection, PgConnection, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect_and_migrate(database_url: &str) -> Result<PgPool> {
    let mut migration_connection = PgConnection::connect(database_url)
        .await
        .context("connect migration database")?;
    sqlx::query("CREATE SCHEMA IF NOT EXISTS company_context_system")
        .execute(&mut migration_connection)
        .await
        .context("create company context system schema")?;
    sqlx::query("SET search_path TO company_context_system, public")
        .execute(&mut migration_connection)
        .await
        .context("set company context migration search path")?;
    MIGRATOR
        .run(&mut migration_connection)
        .await
        .context("run company context migrations")?;
    migration_connection.close().await?;

    let options = PgConnectOptions::from_str(database_url).context("parse DATABASE_URL")?;
    PgPoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await
        .context("connect company context database")
}

pub async fn ready(pool: &PgPool) -> bool {
    sqlx::query_scalar::<_, i32>(
        "SELECT 1 FROM company_context_data.google_drive_documents LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .is_ok()
}
