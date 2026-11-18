//! Schema adoption (RFC 0006): fresh installs, adopting a Rails-managed
//! database at the baseline, refusing anything else, and a fresh install
//! matching the schema the baseline migration creates.
//!
//! Needs `CONSOLE_DB_TEST_DATABASE_URL` (see `centaur_console_db::testing`).

use centaur_console_db::{
    migrate::{
        MIGRATOR, MigrationError, MigrationOutcome, RAILS_BASELINE_VERSION, is_current, migrate,
    },
    testing::TestDb,
};
use sqlx::{Executor, PgPool};

const FINGERPRINT_SQL: &str = include_str!("../sql/schema_fingerprint.sql");
const BASELINE_SQL: &str = include_str!("../migrations/0001_baseline.sql");

async fn fingerprint(pool: &PgPool) -> String {
    let lines: Vec<String> = sqlx::query_scalar(FINGERPRINT_SQL)
        .fetch_all(pool)
        .await
        .unwrap();
    lines.join("\n") + "\n"
}

/// A database exactly as Rails leaves it at the baseline version: the
/// baseline SQL (a dump of a Rails-migrated database) without SQLx's table.
async fn rails_managed() -> Option<TestDb> {
    let db = TestDb::empty().await?;
    db.pool.execute(BASELINE_SQL).await.unwrap();
    Some(db)
}

/// The schema the committed baseline migration creates, as a fingerprint.
async fn baseline_fingerprint() -> String {
    let db = rails_managed().await.expect("test database");
    let fingerprint = fingerprint(&db.pool).await;
    db.drop_database().await;
    fingerprint
}

async fn sqlx_table_exists(pool: &PgPool) -> bool {
    sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_fresh_database_gets_the_whole_schema_matching_the_baseline() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    assert!(!is_current(&db.pool).await.unwrap());
    assert_eq!(
        migrate(&db.pool).await.unwrap(),
        MigrationOutcome::Installed
    );
    assert!(is_current(&db.pool).await.unwrap());

    let expected = baseline_fingerprint().await;
    assert_eq!(fingerprint(&db.pool).await, expected);

    let version: String = sqlx::query_scalar("SELECT max(version) FROM schema_migrations")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(version, RAILS_BASELINE_VERSION);
    let environment: String =
        sqlx::query_scalar("SELECT value FROM ar_internal_metadata WHERE key = 'environment'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(environment, "production");

    // Idempotent: a second start changes nothing.
    assert_eq!(migrate(&db.pool).await.unwrap(), MigrationOutcome::UpToDate);
    assert_eq!(fingerprint(&db.pool).await, expected);
    db.drop_database().await;
}

#[tokio::test]
async fn a_rails_database_at_the_baseline_is_adopted_without_rerunning_it() {
    let Some(db) = rails_managed().await else {
        return;
    };
    sqlx::query(
        "INSERT INTO users (email, admin, status, created_at, updated_at) \
         VALUES ('kept@example.test', true, 'active', now(), now())",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    assert!(!sqlx_table_exists(&db.pool).await);

    assert_eq!(
        migrate(&db.pool).await.unwrap(),
        MigrationOutcome::AdoptedRails
    );

    // Recorded, not run: the existing row survives and the schema is the same.
    let kept: i64 =
        sqlx::query_scalar("SELECT count(*) FROM users WHERE email = 'kept@example.test'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(kept, 1);
    assert_eq!(fingerprint(&db.pool).await, baseline_fingerprint().await);
    let recorded: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    let embedded: Vec<(i64, bool, Vec<u8>)> = MIGRATOR
        .iter()
        .map(|migration| (migration.version, true, migration.checksum.to_vec()))
        .collect();
    assert_eq!(recorded, embedded);
    assert!(is_current(&db.pool).await.unwrap());

    assert_eq!(migrate(&db.pool).await.unwrap(), MigrationOutcome::UpToDate);
    db.drop_database().await;
}

#[tokio::test]
async fn any_other_rails_version_is_refused() {
    let cases = [
        // An older Rails schema: the Rails image must finish migrating.
        "DELETE FROM schema_migrations WHERE version = '20261109021725'",
        // A Rails migration past the freeze.
        "INSERT INTO schema_migrations (version) VALUES ('20261201000000')",
        // A schema_migrations table with no versions.
        "DELETE FROM schema_migrations",
    ];
    for setup in cases {
        let Some(db) = rails_managed().await else {
            return;
        };
        db.pool.execute(setup).await.unwrap();
        let error = migrate(&db.pool).await.expect_err(setup);
        assert!(
            matches!(error, MigrationError::UnsupportedRailsVersion(_)),
            "{setup}: {error}"
        );
        assert!(
            error.to_string().contains(RAILS_BASELINE_VERSION),
            "{error}"
        );
        assert!(
            !sqlx_table_exists(&db.pool).await,
            "{setup}: nothing recorded"
        );
        db.drop_database().await;
    }
}

#[tokio::test]
async fn an_installed_database_with_rails_migrations_past_the_freeze_is_refused() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    migrate(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO schema_migrations (version) VALUES ('20261201000000')")
        .execute(&db.pool)
        .await
        .unwrap();
    assert!(matches!(
        migrate(&db.pool).await,
        Err(MigrationError::UnsupportedRailsVersion(Some(version))) if version == "20261201000000"
    ));
    db.drop_database().await;
}

#[tokio::test]
async fn concurrent_starts_adopt_once() {
    let Some(db) = rails_managed().await else {
        return;
    };
    let results = futures_util::future::join_all((0..4).map(|_| migrate(&db.pool))).await;
    let outcomes: Vec<_> = results.into_iter().map(Result::unwrap).collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == MigrationOutcome::AdoptedRails)
            .count(),
        1,
        "{outcomes:?}"
    );
    db.drop_database().await;
}
