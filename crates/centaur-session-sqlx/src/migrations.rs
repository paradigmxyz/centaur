//! Embedded schema migrations.
//!
//! Core migrations work on any PostgreSQL server with pgvector. Keyword search
//! indexes come from exactly one text-search backend per database. Each
//! backend's migrations share the core version sequence and are merged into it
//! by version, so every search migration runs right after the core migrations
//! it was written against, on fresh and existing databases alike.

use std::{borrow::Cow, fmt, future::Future, pin::Pin, str::FromStr};

use sqlx::{
    PgConnection,
    error::BoxDynError,
    migrate::{Migration, MigrationSource, Migrator},
};

use crate::SessionStoreError;

// The API binary embeds these migrations at compile time.
static CORE_MIGRATOR: Migrator = sqlx::migrate!("./migrations");
static PARADEDB_MIGRATOR: Migrator = sqlx::migrate!("./search-migrations/paradedb");
static POSTGRES_MIGRATOR: Migrator = sqlx::migrate!("./search-migrations/postgres");

/// SHA-384 checksums that databases recorded for core migrations before their
/// ParadeDB statements moved to the `paradedb` text-search backend. Presenting
/// the recorded checksum keeps those databases valid without rewriting
/// `_sqlx_migrations`; fresh databases record the same values.
const LEGACY_CHECKSUMS: [(i64, &str); 6] = [
    (
        12,
        "4c5484e974feda89ced85376ababe05457ebc1e9d2756b835c39fcf9c010a318f22166988da9f82d6b10da4587d1de38",
    ),
    (
        28,
        "fd4aaae62dd5fd310ac028c335dfda19eb2169c199b738c068fb1ef0716426ff00623382f18be5f1a33ef4f7ebf109e3",
    ),
    (
        29,
        "c472c35a307682a84bee8172af27348457442b6d7598dab1eaa23ac8068c79d33c544fa078ded09b130792ab673f0142",
    ),
    (
        30,
        "2feeec8685e4e70ab831710247fe775c160fb97a7d5802cdfc9abaaebca914b5b6ddd49640cb5779bdf256d6595ea1b0",
    ),
    (
        40,
        "65cbd5bafcfd4e124d51bd99cc501fee923e87882d1032dad34f4cfba3fd78b5d4869b61bb765f8d76310e520f44cac2",
    ),
    (
        45,
        "8e1ee367cc62f1ffde0b4fad158291266ef78598ab613136822c9703d4fe049b883fd588328b894cb05480b0765b393e",
    ),
];

/// Tables that core migrations gave BM25 indexes before the backends split.
const LEGACY_BM25_TABLES: [&str; 5] = [
    "company_context_documents",
    "google_docs_context_documents",
    "granola_context_documents",
    "slack_private_context_documents",
    "slack_private_conversation_context_documents",
];

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

/// Core migrations merged with one text-search backend, in version order.
pub fn migration_list(backend: TextSearchBackend) -> Vec<Migration> {
    let mut migrations: Vec<Migration> = CORE_MIGRATOR
        .iter()
        .cloned()
        .map(|mut migration| {
            if let Some((_, checksum)) = LEGACY_CHECKSUMS
                .iter()
                .find(|(version, _)| *version == migration.version)
            {
                migration.checksum =
                    Cow::Owned(hex::decode(checksum).expect("legacy checksums are valid hex"));
            }
            migration
        })
        .chain(backend.migrator().iter().cloned())
        .collect();
    migrations.sort_by_key(|migration| migration.version);
    migrations
}

/// Apply pending migrations for `backend`.
///
/// Fails without changing the database when it was migrated with another
/// text-search backend, or when `postgres` is configured for a database that
/// still has BM25 indexes.
pub async fn migrate(
    conn: &mut PgConnection,
    backend: TextSearchBackend,
) -> Result<(), SessionStoreError> {
    ensure_text_search_backend(conn, backend).await?;
    Migrator::new(MigrationList(migration_list(backend)))
        .await?
        .run(conn)
        .await?;
    Ok(())
}

async fn ensure_text_search_backend(
    conn: &mut PgConnection,
    backend: TextSearchBackend,
) -> Result<(), SessionStoreError> {
    if backend == TextSearchBackend::Postgres {
        // Databases migrated before the backends split carry BM25 indexes from
        // core migrations. They can be large, so never drop them implicitly;
        // such a database keeps the paradedb backend.
        let indexes: Vec<String> = sqlx::query_scalar(
            "select index.relname::text
             from pg_index
             join pg_class index on index.oid = pg_index.indexrelid
             join pg_class tables on tables.oid = pg_index.indrelid
             join pg_am am on am.oid = index.relam
             where am.amname = 'bm25'
               and tables.relnamespace = current_schema()::regnamespace
               and tables.relname = any($1)
             order by 1",
        )
        .bind(&LEGACY_BM25_TABLES[..])
        .fetch_all(&mut *conn)
        .await?;
        if !indexes.is_empty() {
            return Err(SessionStoreError::Bm25IndexesPresent { indexes });
        }
    }
    let tracked: bool = sqlx::query_scalar("select to_regclass('_sqlx_migrations') is not null")
        .fetch_one(&mut *conn)
        .await?;
    if !tracked {
        return Ok(());
    }
    let applied: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("select version, checksum from _sqlx_migrations")
            .fetch_all(&mut *conn)
            .await?;
    for (version, checksum) in applied {
        let matches = |candidate: TextSearchBackend| {
            candidate
                .migrator()
                .iter()
                .any(|migration| migration.version == version && *migration.checksum == *checksum)
        };
        if matches(backend) {
            continue;
        }
        if let Some(applied) = TextSearchBackend::ALL
            .into_iter()
            .find(|candidate| *candidate != backend && matches(*candidate))
        {
            return Err(SessionStoreError::TextSearchBackendMismatch {
                configured: backend,
                applied,
            });
        }
    }
    Ok(())
}

#[derive(Debug)]
struct MigrationList(Vec<Migration>);

impl<'s> MigrationSource<'s> for MigrationList {
    fn resolve(
        self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Migration>, BoxDynError>> + Send + 's>> {
        Box::pin(async move { Ok(self.0) })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn versions(migrator: &Migrator) -> BTreeSet<i64> {
        migrator.iter().map(|migration| migration.version).collect()
    }

    #[test]
    fn search_versions_do_not_collide_with_core_versions() {
        let core = versions(&CORE_MIGRATOR);
        assert_eq!(core.len(), CORE_MIGRATOR.iter().count());
        for backend in TextSearchBackend::ALL {
            let search = versions(backend.migrator());
            assert_eq!(search.len(), backend.migrator().iter().count());
            let collisions: Vec<_> = core.intersection(&search).collect();
            assert!(
                collisions.is_empty(),
                "{backend} migrations reuse core versions {collisions:?}"
            );
        }
    }

    #[test]
    fn every_backend_carries_every_search_version() {
        let [first, rest @ ..] = TextSearchBackend::ALL;
        for backend in rest {
            assert_eq!(
                versions(first.migrator()),
                versions(backend.migrator()),
                "{first} and {backend} migrations must share versions; add a no-op migration where nothing changes"
            );
        }
    }

    #[test]
    fn merged_list_is_ordered_and_complete() {
        for backend in TextSearchBackend::ALL {
            let merged = migration_list(backend);
            assert!(
                merged
                    .windows(2)
                    .all(|pair| pair[0].version < pair[1].version)
            );
            assert_eq!(
                merged.len(),
                CORE_MIGRATOR.iter().count() + backend.migrator().iter().count()
            );
        }
    }

    #[test]
    fn legacy_checksums_replace_only_edited_core_migrations() {
        let core = versions(&CORE_MIGRATOR);
        for (version, _) in LEGACY_CHECKSUMS {
            assert!(
                core.contains(&version),
                "legacy version {version} is not a core migration"
            );
        }
        let merged = migration_list(TextSearchBackend::Postgres);
        for (migration, original) in merged
            .iter()
            .filter(|migration| core.contains(&migration.version))
            .zip(CORE_MIGRATOR.iter())
        {
            let legacy = LEGACY_CHECKSUMS
                .iter()
                .any(|(version, _)| *version == migration.version);
            assert_eq!(migration.checksum != original.checksum, legacy);
        }
    }
}
