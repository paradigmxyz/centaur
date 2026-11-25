//! Embedded schema migrations and the database connection.
//!
//! Keyword search runs on one of two text-search backends, each with its own
//! complete migration tree: `migrations/paradedb` (ParadeDB BM25 indexes) and
//! `migrations/postgres` (built-in full-text search, for servers without
//! pg_search such as RDS and Cloud SQL). The trees share versions and are
//! identical except for the migrations that create or rename keyword search
//! indexes, which a test lists; a new migration goes into both. A database
//! keeps the backend it was first migrated with.

use std::{fmt, str::FromStr};

use anyhow::{Context, Result, bail};
use sqlx::{
    Connection, PgConnection, PgPool,
    migrate::Migrator,
    postgres::{PgConnectOptions, PgPoolOptions},
};

static PARADEDB_MIGRATOR: Migrator = sqlx::migrate!("./migrations/paradedb");
static POSTGRES_MIGRATOR: Migrator = sqlx::migrate!("./migrations/postgres");

/// Keyword search implementation installed in a database.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextSearchBackend {
    /// ParadeDB `pg_search` BM25 indexes.
    Paradedb,
    /// Built-in PostgreSQL full-text search (`tsvector` and GIN).
    Postgres,
}

impl TextSearchBackend {
    pub const ALL: [Self; 2] = [Self::Paradedb, Self::Postgres];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Paradedb => "paradedb",
            Self::Postgres => "postgres",
        }
    }

    fn migrator(self) -> &'static Migrator {
        match self {
            Self::Paradedb => &PARADEDB_MIGRATOR,
            Self::Postgres => &POSTGRES_MIGRATOR,
        }
    }
}

impl fmt::Display for TextSearchBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TextSearchBackend {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|backend| backend.as_str() == value)
            .ok_or_else(|| {
                format!("unknown text search backend {value:?}; expected paradedb or postgres")
            })
    }
}

pub async fn connect_and_migrate(database_url: &str, backend: TextSearchBackend) -> Result<PgPool> {
    let mut migration_connection = PgConnection::connect(database_url)
        .await
        .context("connect migration database")?;
    migrate(&mut migration_connection, backend).await?;
    migration_connection.close().await?;

    let options = PgConnectOptions::from_str(database_url).context("parse DATABASE_URL")?;
    PgPoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await
        .context("connect company context database")
}

/// Apply pending migrations for `backend`, failing without changing the
/// database when it was migrated with the other backend.
pub async fn migrate(conn: &mut PgConnection, backend: TextSearchBackend) -> Result<()> {
    sqlx::query("CREATE SCHEMA IF NOT EXISTS company_context_system")
        .execute(&mut *conn)
        .await
        .context("create company context system schema")?;
    sqlx::query("SET search_path TO company_context_system, public")
        .execute(&mut *conn)
        .await
        .context("set company context migration search path")?;
    ensure_text_search_backend(conn, backend).await?;
    backend
        .migrator()
        .run(conn)
        .await
        .context("run company context migrations")?;
    Ok(())
}

/// SQLx would report the other tree's migrations as modified; name the
/// backend mismatch instead.
async fn ensure_text_search_backend(
    conn: &mut PgConnection,
    backend: TextSearchBackend,
) -> Result<()> {
    let tracked: bool = sqlx::query_scalar(
        "select to_regclass('company_context_system._sqlx_migrations') is not null",
    )
    .fetch_one(&mut *conn)
    .await?;
    if !tracked {
        return Ok(());
    }
    let applied: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("select version, checksum from company_context_system._sqlx_migrations")
            .fetch_all(&mut *conn)
            .await?;
    let matches = |candidate: TextSearchBackend, version: i64, checksum: &[u8]| {
        candidate
            .migrator()
            .iter()
            .any(|migration| migration.version == version && *migration.checksum == *checksum)
    };
    for (version, checksum) in &applied {
        if matches(backend, *version, checksum) {
            continue;
        }
        if let Some(applied) = TextSearchBackend::ALL
            .into_iter()
            .find(|candidate| *candidate != backend && matches(*candidate, *version, checksum))
        {
            bail!(
                "text search backend {backend} is configured, but the database was migrated \
                 with {applied}; a database keeps the backend it was first migrated with"
            );
        }
    }
    Ok(())
}

pub async fn ready(pool: &PgPool) -> bool {
    sqlx::query_scalar::<_, i32>(
        "SELECT 1 FROM company_context_data.google_drive_documents LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .is_ok()
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use sqlx::Executor;

    use super::*;
    use crate::test_support::MIGRATIONS;

    /// Migrations whose SQL differs between the backends' trees, because they
    /// create or rename keyword search indexes.
    const DIVERGENT_VERSIONS: [i64; 7] = [1, 4, 7, 8, 13, 14, 15];

    /// Tables keyword search reads; each backend must index every one of them.
    const SEARCHABLE_TABLES: [&str; 6] = [
        "google_drive_documents",
        "granola_documents",
        "slack_documents",
        "slack_file_documents",
        "google_calendar_documents",
        "custom_document_chunks",
    ];

    const BM25_INDEX: &str = "% USING bm25 %";
    const SEARCH_VECTOR_INDEX: &str = "% USING gin (search_vector)";

    #[test]
    fn migration_trees_match_except_divergent_versions() {
        let paradedb: Vec<_> = PARADEDB_MIGRATOR.iter().collect();
        let postgres: Vec<_> = POSTGRES_MIGRATOR.iter().collect();
        assert_eq!(
            paradedb
                .iter()
                .map(|migration| (migration.version, &migration.description))
                .collect::<Vec<_>>(),
            postgres
                .iter()
                .map(|migration| (migration.version, &migration.description))
                .collect::<Vec<_>>(),
            "both migration trees need every migration, under the same version and name"
        );
        for (paradedb, postgres) in paradedb.iter().zip(&postgres) {
            let divergent = DIVERGENT_VERSIONS.contains(&paradedb.version);
            assert_eq!(
                paradedb.checksum != postgres.checksum,
                divergent,
                "migration {} must be identical in both trees unless it is in DIVERGENT_VERSIONS, \
                 and listed only while it differs",
                paradedb.version
            );
        }
    }

    struct Server {
        url: String,
        backends: Vec<TextSearchBackend>,
    }

    struct Database {
        name: String,
        conn: PgConnection,
    }

    impl Server {
        async fn connect() -> Option<Self> {
            let Ok(url) = std::env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
                eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a Postgres URL");
                return None;
            };
            let mut admin = PgConnection::connect(&url).await.unwrap();
            let pg_search: bool = sqlx::query_scalar(
                "select exists (select 1 from pg_available_extensions where name = 'pg_search')",
            )
            .fetch_one(&mut admin)
            .await
            .unwrap();
            admin.close().await.unwrap();
            let backends = TextSearchBackend::ALL
                .into_iter()
                .filter(|backend| pg_search || *backend != TextSearchBackend::Paradedb)
                .collect();
            Some(Self { url, backends })
        }

        async fn create(&self) -> Database {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let name = format!("company_context_migrations_{}_{nanos}", std::process::id());
            let mut admin = PgConnection::connect(&self.url).await.unwrap();
            admin
                .execute(format!(r#"create database "{name}""#).as_str())
                .await
                .unwrap();
            admin.close().await.unwrap();
            let options = PgConnectOptions::from_str(&self.url)
                .unwrap()
                .database(&name);
            let conn = PgConnection::connect_with(&options).await.unwrap();
            Database { name, conn }
        }

        async fn drop(&self, database: Database) {
            database.conn.close().await.unwrap();
            let mut admin = PgConnection::connect(&self.url).await.unwrap();
            // pg_search background mergers can outlive a plain DROP DATABASE.
            admin
                .execute(
                    format!(
                        r#"drop database if exists "{}" with (force)"#,
                        database.name
                    )
                    .as_str(),
                )
                .await
                .unwrap();
            admin.close().await.unwrap();
        }
    }

    /// Whether `table` has an index whose definition matches `pattern`.
    async fn indexed(conn: &mut PgConnection, table: &str, pattern: &str) -> bool {
        sqlx::query_scalar(
            "select exists (
                select 1 from pg_indexes
                where schemaname = 'company_context_data' and tablename = $1 and indexdef like $2
            )",
        )
        .bind(table)
        .bind(pattern)
        .fetch_one(conn)
        .await
        .unwrap()
    }

    async fn migration_rows(conn: &mut PgConnection) -> Vec<(i64, Vec<u8>)> {
        sqlx::query_as(
            "select version, checksum from company_context_system._sqlx_migrations order by 1",
        )
        .fetch_all(conn)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn fresh_databases_migrate_and_index_every_searchable_table() {
        let Some(server) = Server::connect().await else {
            return;
        };
        let _guard = MIGRATIONS.lock().await;
        for backend in server.backends.clone() {
            let mut database = server.create().await;
            migrate(&mut database.conn, backend).await.unwrap();
            // A second run finds nothing to apply.
            migrate(&mut database.conn, backend).await.unwrap();
            let (expected, other) = match backend {
                TextSearchBackend::Paradedb => (BM25_INDEX, SEARCH_VECTOR_INDEX),
                TextSearchBackend::Postgres => (SEARCH_VECTOR_INDEX, BM25_INDEX),
            };
            for table in SEARCHABLE_TABLES {
                assert!(
                    indexed(&mut database.conn, table, expected).await,
                    "{backend} migrations do not index {table}"
                );
                assert!(
                    !indexed(&mut database.conn, table, other).await,
                    "{backend} migrations create the other backend's index on {table}"
                );
            }
            server.drop(database).await;
        }
    }

    #[tokio::test]
    async fn databases_keep_their_text_search_backend() {
        let Some(server) = Server::connect().await else {
            return;
        };
        let _guard = MIGRATIONS.lock().await;
        for backend in server.backends.clone() {
            for other in TextSearchBackend::ALL {
                if other == backend {
                    continue;
                }
                let mut database = server.create().await;
                migrate(&mut database.conn, backend).await.unwrap();
                let before = migration_rows(&mut database.conn).await;
                let error = migrate(&mut database.conn, other)
                    .await
                    .expect_err("switching backends must fail");
                assert!(
                    format!("{error:#}").contains(&format!("migrated with {backend}")),
                    "{error:#}"
                );
                assert_eq!(migration_rows(&mut database.conn).await, before);
                server.drop(database).await;
            }
        }
    }
}
