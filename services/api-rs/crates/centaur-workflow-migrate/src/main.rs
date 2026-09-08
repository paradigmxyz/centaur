//! Executes workflow-owned SQLx migrations. One JSON request on stdin, one
//! JSON result on stdout. The caller keeps stdin open until the runner exits.

use std::{collections::HashSet, io::Read, path::PathBuf, process::ExitCode, time::Duration};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sqlx::{
    Connection, PgConnection,
    migrate::{Migrate, Migrator},
};

#[derive(Deserialize)]
struct Request {
    directory: PathBuf,
    schema: String,
    lock_timeout: f64,
}

fn main() -> ExitCode {
    match run() {
        Ok(applied) => {
            println!("{}", serde_json::json!({"applied": applied}));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<Vec<i64>> {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let request: Request = serde_json::from_str(&line).context("invalid migration request")?;

    // Closing the parent pipe (including abrupt host death) must not leave an
    // orphan runner holding a database lock or applying more migrations.
    std::thread::spawn(|| match std::io::stdin().read(&mut [0]) {
        Ok(_) | Err(_) => std::process::exit(1),
    });

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(migrate(request))
}

async fn migrate(request: Request) -> Result<Vec<i64>> {
    ensure!(
        !request.schema.is_empty() && request.schema.len() <= 63 && !request.schema.contains('\0'),
        "schema must contain 1 to 63 UTF-8 bytes and no NUL"
    );
    ensure!(
        request.lock_timeout.is_finite() && request.lock_timeout > 0.0,
        "lock_timeout must be positive and finite"
    );
    let timeout = Duration::try_from_secs_f64(request.lock_timeout)?;
    let mut migrator = Migrator::new(request.directory.as_path()).await?;
    // A still-running older bundle may not include newly applied migrations.
    migrator.set_ignore_missing(true);
    // Take SQLx's own lock before creating a schema or its history table.
    migrator.set_locking(false);
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is not configured")?;
    // A dedicated connection is dropped on every error, releasing session locks.
    let mut connection = PgConnection::connect(&database_url)
        .await
        .context("database connection failed")?;
    // PostgreSQL otherwise may finish a long query before noticing that a
    // cancelled or killed runner disconnected. Bound that detection delay.
    sqlx::query("SET client_connection_check_interval = '1s'")
        .execute(&mut connection)
        .await?;
    tokio::time::timeout(timeout, connection.lock())
        .await
        .context("timed out waiting for the SQLx migration lock")??;

    let quoted: String = sqlx::query_scalar("SELECT pg_catalog.quote_ident($1)")
        .bind(&request.schema)
        .fetch_one(&mut connection)
        .await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspname = $1)",
    )
    .bind(&request.schema)
    .fetch_one(&mut connection)
    .await?;
    // IF NOT EXISTS still requires database CREATE permission on PostgreSQL.
    if !exists {
        sqlx::query(&format!("CREATE SCHEMA {quoted}"))
            .execute(&mut connection)
            .await?;
    }
    // Exclude public: otherwise an existing control-plane _sqlx_migrations
    // table could be resolved before this schema gets its own history table.
    sqlx::query("SELECT pg_catalog.set_config('search_path', $1, false)")
        .bind(&quoted)
        .execute(&mut connection)
        .await?;
    connection.ensure_migrations_table().await?;
    let previous: HashSet<_> = connection
        .list_applied_migrations()
        .await?
        .into_iter()
        .map(|migration| migration.version)
        .collect();
    migrator.run_direct(&mut connection).await?;
    let applied = migrator
        .iter()
        .filter(|migration| {
            migration.migration_type.is_up_migration() && !previous.contains(&migration.version)
        })
        .map(|migration| migration.version)
        .collect();
    connection.close().await?;
    Ok(applied)
}
