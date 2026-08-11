use std::{
    env,
    error::Error,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use centaur_session_sqlx::{PgSessionStore, SessionStoreError};
use sqlx::{
    Connection, Executor, PgConnection,
    postgres::{PgConnectOptions, PgPoolOptions},
};

#[tokio::test]
#[ignore = "requires SESSION_SQLX_TEST_DATABASE_URL"]
async fn embedded_migrator_accepts_future_versions_but_checks_known_versions()
-> Result<(), Box<dyn Error>> {
    let database_url = test_database_url()?;
    let mut admin_conn = PgConnection::connect(&database_url).await?;
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let name = format!(
        "centaur_migration_rollback_{}_{}",
        std::process::id(),
        nanos
    );
    admin_conn
        .execute(format!(r#"create database "{name}""#).as_str())
        .await?;

    let result = async {
        let options = PgConnectOptions::from_str(&database_url)?.database(&name);
        let pool = PgPoolOptions::new().connect_with(options).await?;
        let store = PgSessionStore::new(pool);
        store.run_migrations().await?;

        sqlx::query(
            r#"
            insert into _sqlx_migrations
                (version, description, installed_on, success, checksum, execution_time)
            values
                (9223372036854775807, 'future migration', now(), true, decode(repeat('00', 48), 'hex'), 0)
            "#,
        )
        .execute(store.pool())
        .await?;
        store.run_migrations().await?;

        sqlx::query("update _sqlx_migrations set checksum = decode(repeat('ff', 48), 'hex') where version = 1")
            .execute(store.pool())
            .await?;
        let error = store.run_migrations().await.unwrap_err();
        assert!(matches!(
            error,
            SessionStoreError::Migrate(sqlx::migrate::MigrateError::VersionMismatch(1))
        ));
        store.pool().close().await;
        Ok::<(), Box<dyn Error>>(())
    }
    .await;

    let drop_result = admin_conn
        .execute(format!(r#"drop database if exists "{name}""#).as_str())
        .await;
    result?;
    drop_result?;
    Ok(())
}

fn test_database_url() -> Result<String, env::VarError> {
    env::var("SESSION_SQLX_TEST_DATABASE_URL")
        .or_else(|_| env::var("SESSION_RUNTIME_TEST_DATABASE_URL"))
}
