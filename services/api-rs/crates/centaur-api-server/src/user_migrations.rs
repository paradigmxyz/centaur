//! Deployment-owned SQLx migrations, completed before workflow workers start.

use std::{borrow::Cow, path::PathBuf};

use sqlx::{
    Connection, PgConnection, PgPool,
    migrate::{Migrate, MigrateError, Migrator},
};
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum UserMigrationError {
    #[error(transparent)]
    Migration(#[from] MigrateError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("cannot read user migrations at {path}: {source}")]
    Directory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("duplicate user migration version {0} across configured directories")]
    DuplicateVersion(i64),
}

pub async fn run(pool: &PgPool, directories: &[PathBuf]) -> Result<(), UserMigrationError> {
    if directories.is_empty() {
        return Ok(());
    }
    let mut migrations = Vec::new();
    for directory in directories {
        match tokio::fs::metadata(directory).await {
            Ok(_) => {
                let source = Migrator::new(directory.as_path()).await?;
                migrations.extend(
                    source
                        .migrations
                        .into_owned()
                        .into_iter()
                        .filter(|m| m.migration_type.is_up_migration()),
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // An overlay need not contain migrations, but an unavailable
                // checkout must not silently unblock workflow workers.
                let parent = directory
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or_else(|| std::path::Path::new("."));
                tokio::fs::metadata(parent).await.map_err(|source| {
                    UserMigrationError::Directory {
                        path: parent.to_owned(),
                        source,
                    }
                })?;
            }
            Err(source) => {
                return Err(UserMigrationError::Directory {
                    path: directory.clone(),
                    source,
                });
            }
        }
    }
    migrations.sort_by_key(|migration| migration.version);
    for pair in migrations.windows(2) {
        if pair[0].version == pair[1].version {
            return Err(UserMigrationError::DuplicateVersion(pair[0].version));
        }
    }
    let mut migrator = Migrator {
        migrations: Cow::Owned(migrations),
        ..Migrator::DEFAULT
    };
    // Older replicas may have a subset of migrations during rolling deploys.
    migrator.set_ignore_missing(true);
    migrator.set_locking(false);

    // Never return a connection with altered search_path or a held lock to the
    // shared application pool. Dropping this dedicated connection releases both.
    let mut connection = PgConnection::connect_with(&pool.connect_options()).await?;
    sqlx::query("SET client_connection_check_interval = '1s'")
        .execute(&mut connection)
        .await?;
    // SQLx's database-wide lock also covers creation of the history schema.
    info!("waiting for user migration lock");
    connection.lock().await?;
    sqlx::query("CREATE SCHEMA IF NOT EXISTS user_migrations")
        .execute(&mut connection)
        .await?;
    // Exclude public so core _sqlx_migrations is never resolved accidentally.
    sqlx::query("SET search_path TO user_migrations")
        .execute(&mut connection)
        .await?;
    info!(count = migrator.iter().len(), "applying user migrations");
    migrator.run_direct(&mut connection).await?;
    connection.close().await?;
    info!("user migrations complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;
    use std::time::Duration;

    #[tokio::test]
    async fn user_migrations_isolate_history_serialize_replicas_and_recover() {
        let Ok(url) = std::env::var("SESSION_SQLX_TEST_DATABASE_URL") else {
            eprintln!("skipping: SESSION_SQLX_TEST_DATABASE_URL is not configured");
            return;
        };
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let database = format!("user_migrations_test_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {database}"))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with((*admin.connect_options()).clone().database(&database))
            .await
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        // Versions sort across overlays rather than following overlay order.
        let dirs = vec![second.clone(), first.clone()];
        std::fs::write(first.join("1_create.sql"),
            "CREATE SCHEMA user_data; CREATE TABLE user_data.cursors (id int PRIMARY KEY, cursor int);").unwrap();
        std::fs::write(
            second.join("2_insert.sql"),
            "INSERT INTO user_data.cursors VALUES (1, 42);",
        )
        .unwrap();
        sqlx::raw_sql("CREATE TABLE public._sqlx_migrations (marker text); INSERT INTO public._sqlx_migrations VALUES ('core')")
            .execute(&pool).await.unwrap();
        assert!(matches!(
            run(&pool, &[tmp.path().join("missing-checkout/migrations")]).await,
            Err(UserMigrationError::Directory { .. })
        ));
        run(&pool, &[tmp.path().join("optional-migrations")])
            .await
            .unwrap();
        let original_path: String = sqlx::query_scalar("SHOW search_path")
            .fetch_one(&pool)
            .await
            .unwrap();
        let (a, b, c) = tokio::join!(run(&pool, &dirs), run(&pool, &dirs), run(&pool, &dirs));
        a.unwrap();
        b.unwrap();
        c.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i32>("SELECT cursor FROM user_data.cursors")
                .fetch_one(&pool)
                .await
                .unwrap(),
            42
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT marker FROM public._sqlx_migrations")
                .fetch_one(&pool)
                .await
                .unwrap(),
            "core"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SHOW search_path")
                .fetch_one(&pool)
                .await
                .unwrap(),
            original_path
        );

        std::fs::write(
            first.join("3_change.sql"),
            "CREATE TABLE user_data.pending (id int); SELECT 1/0;",
        )
        .unwrap();
        assert!(run(&pool, &dirs).await.is_err());
        assert_eq!(history(&pool).await, vec![1, 2]);
        assert!(
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT to_regclass('user_data.pending')::text"
            )
            .fetch_one(&pool)
            .await
            .unwrap()
            .is_none()
        );
        std::fs::write(
            first.join("3_change.sql"),
            "CREATE TABLE user_data.pending (id int);",
        )
        .unwrap();
        run(&pool, &dirs).await.unwrap();
        assert_eq!(history(&pool).await, vec![1, 2, 3]);

        std::fs::write(second.join("3_duplicate.sql"), "SELECT 1;").unwrap();
        assert!(matches!(
            run(&pool, &dirs).await,
            Err(UserMigrationError::DuplicateVersion(3))
        ));
        std::fs::remove_file(second.join("3_duplicate.sql")).unwrap();
        std::fs::write(first.join("3_change.sql"), "SELECT 1;").unwrap();
        assert!(matches!(
            run(&pool, &dirs).await,
            Err(UserMigrationError::Migration(
                MigrateError::VersionMismatch(3)
            ))
        ));
        // Older replicas may carry fewer migration files.
        std::fs::remove_file(first.join("3_change.sql")).unwrap();
        run(&pool, &dirs).await.unwrap();

        std::fs::write(
            first.join("4_wait.sql"),
            "CREATE TABLE user_data.cancelled (id int); SELECT pg_sleep(30);",
        )
        .unwrap();
        let task_pool = pool.clone();
        let task_dirs = dirs.clone();
        let task = tokio::spawn(async move { run(&task_pool, &task_dirs).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let sleeping: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event = 'PgSleep')").fetch_one(&pool).await.unwrap();
                if sleeping { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        std::fs::write(
            first.join("4_wait.sql"),
            "CREATE TABLE user_data.cancelled (id int);",
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), run(&pool, &dirs))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(history(&pool).await, vec![1, 2, 3, 4]);

        pool.close().await;
        sqlx::query(&format!("DROP DATABASE {database}"))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }

    async fn history(pool: &PgPool) -> Vec<i64> {
        sqlx::query_scalar("SELECT version FROM user_migrations._sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await
            .unwrap()
    }
}
