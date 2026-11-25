//! The shared bootstrap for the database-backed Console tests.
//!
//! Set `CONSOLE_DB_TEST_DATABASE_URL` to a database migrated with
//! `cargo run -p centaur-console-db --example migrate -- <url>`. Without the variable the tests skip locally and fail
//! in CI. Each test inserts uniquely named rows, so tests can share one database
//! and run in parallel; the fixtures that seed those rows stay per file.

use std::env;

use sqlx::{PgPool, postgres::PgPoolOptions};

/// A pool for the test database, or `None` when the test should skip.
pub async fn database() -> Option<PgPool> {
    let Ok(url) = env::var("CONSOLE_DB_TEST_DATABASE_URL") else {
        assert!(
            env::var_os("CI").is_none(),
            "CONSOLE_DB_TEST_DATABASE_URL must be set in CI"
        );
        eprintln!("skipping: set CONSOLE_DB_TEST_DATABASE_URL to a database with Console's schema");
        return None;
    };
    Some(
        PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect to the test database"),
    )
}
