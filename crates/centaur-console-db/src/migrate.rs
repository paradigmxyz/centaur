//! Schema ownership (RFC 0006, Decision 2).
//!
//! The console schema is SQLx's from here on. `migrations/0001_baseline.sql`
//! is the Rails schema at [`RAILS_BASELINE_VERSION`]. [`migrate`] decides
//! what to do with a database:
//!
//! - already adopted (`_sqlx_migrations` records the baseline): apply any
//!   later SQLx migrations;
//! - fresh (no `schema_migrations`): run every SQLx migration, baseline
//!   included;
//! - Rails-managed at exactly the baseline version: record the baseline as
//!   applied without running it, then apply later migrations;
//! - anything else (an older Rails schema, or Rails migrations past the
//!   freeze): refuse, so the Rails image finishes migrating first.
//!
//! All of it runs under SQLx's migration advisory lock on one connection, so
//! replicas starting together adopt the database once.

use std::fmt;

use sqlx::{
    Acquire, PgConnection, PgPool,
    migrate::{Migrate, MigrateError, Migrator},
};

/// The last Rails migration; the baseline reproduces the schema it left.
pub const RAILS_BASELINE_VERSION: &str = "20261109021725";

/// SQLx's version for the baseline migration file.
pub const BASELINE_MIGRATION_VERSION: i64 = 1;

/// The console's embedded SQLx migrations.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// What [`migrate`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// A fresh database: every migration ran.
    Installed,
    /// A Rails-managed database at the baseline: the baseline was recorded
    /// without running, later migrations (if any) ran.
    AdoptedRails,
    /// SQLx already owned the schema; pending migrations (if any) ran.
    UpToDate,
}

#[derive(Debug)]
pub enum MigrationError {
    /// The database is Rails-managed at another version. `None` means
    /// `schema_migrations` exists but is empty.
    UnsupportedRailsVersion(Option<String>),
    Sqlx(MigrateError),
    Database(sqlx::Error),
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedRailsVersion(found) => write!(
                formatter,
                "the console database is managed by Rails at schema version {}, but this \
                 release adopts only version {RAILS_BASELINE_VERSION}; finish migrating with \
                 the Rails console image first",
                found.as_deref().unwrap_or("(none)")
            ),
            Self::Sqlx(error) => write!(formatter, "console migration failed: {error}"),
            Self::Database(error) => write!(formatter, "console migration failed: {error}"),
        }
    }
}

impl std::error::Error for MigrationError {}

impl From<MigrateError> for MigrationError {
    fn from(error: MigrateError) -> Self {
        Self::Sqlx(error)
    }
}

impl From<sqlx::Error> for MigrationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

/// Brings the console schema up to date (see the module docs).
pub async fn migrate(pool: &PgPool) -> Result<MigrationOutcome, MigrationError> {
    let mut conn = pool.acquire().await?;
    let conn = conn.acquire().await?;
    conn.lock().await?;
    let result = migrate_locked(conn).await;
    let unlocked = conn.unlock().await;
    let outcome = result?;
    unlocked?;
    Ok(outcome)
}

/// Whether every embedded migration is recorded as applied. Used to skip
/// bootstrap on a process that does not migrate (`worker`), as Rails'
/// initializer skips it while migrations are pending.
pub async fn is_current(pool: &PgPool) -> Result<bool, MigrationError> {
    let mut conn = pool.acquire().await?;
    if !table_exists(&mut conn, "_sqlx_migrations").await? {
        return Ok(false);
    }
    let applied = conn.list_applied_migrations().await?;
    Ok(MIGRATOR
        .iter()
        .filter(|migration| migration.migration_type.is_up_migration())
        .all(|migration| applied.iter().any(|done| done.version == migration.version)))
}

async fn migrate_locked(conn: &mut PgConnection) -> Result<MigrationOutcome, MigrationError> {
    let rails_version = rails_schema_version(conn).await?;
    let adopted = baseline_recorded(conn).await?;

    let outcome = match (&rails_version, adopted) {
        // The baseline's own rows make an installed database look like Rails
        // at the baseline; anything else means Rails migrated past the freeze.
        (Some(Some(version)), _) if version != RAILS_BASELINE_VERSION => {
            return Err(MigrationError::UnsupportedRailsVersion(Some(
                version.clone(),
            )));
        }
        (Some(None), _) => return Err(MigrationError::UnsupportedRailsVersion(None)),
        (_, true) => MigrationOutcome::UpToDate,
        (None, false) => MigrationOutcome::Installed,
        (Some(Some(_)), false) => {
            record_baseline(conn).await?;
            MigrationOutcome::AdoptedRails
        }
    };
    // Takes the advisory lock again on this connection; Postgres advisory
    // locks are re-entrant per session.
    MIGRATOR.run(&mut *conn).await?;
    Ok(outcome)
}

/// `None`: no `schema_migrations` table. `Some(None)`: the table is empty.
async fn rails_schema_version(
    conn: &mut PgConnection,
) -> Result<Option<Option<String>>, MigrationError> {
    if !table_exists(conn, "schema_migrations").await? {
        return Ok(None);
    }
    let version: Option<String> =
        sqlx::query_scalar("SELECT max(version) FROM public.schema_migrations")
            .fetch_one(&mut *conn)
            .await?;
    Ok(Some(version))
}

async fn baseline_recorded(conn: &mut PgConnection) -> Result<bool, MigrationError> {
    if !table_exists(conn, "_sqlx_migrations").await? {
        return Ok(false);
    }
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM _sqlx_migrations WHERE version = $1 AND success)",
    )
    .bind(BASELINE_MIGRATION_VERSION)
    .fetch_one(&mut *conn)
    .await?)
}

async fn record_baseline(conn: &mut PgConnection) -> Result<(), MigrationError> {
    let baseline = MIGRATOR
        .iter()
        .find(|migration| migration.version == BASELINE_MIGRATION_VERSION)
        .expect("the baseline migration is embedded");
    conn.ensure_migrations_table().await?;
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES ($1, $2, TRUE, $3, 0) ON CONFLICT (version) DO NOTHING",
    )
    .bind(baseline.version)
    .bind(&*baseline.description)
    .bind(&*baseline.checksum)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn table_exists(conn: &mut PgConnection, table: &str) -> Result<bool, MigrationError> {
    Ok(
        sqlx::query_scalar("SELECT to_regclass('public.' || $1) IS NOT NULL")
            .bind(table)
            .fetch_one(&mut *conn)
            .await?,
    )
}
